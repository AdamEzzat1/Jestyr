//! Memory layout analysis (roadmap workstream **L**, increment 1).
//!
//! Computes the size, alignment, field offsets and **padding waste** of every declared
//! type, and renders them as a report (`jestyrc layout <file>`).
//!
//! ## Why analysis before any reordering
//! L's headline features — field reordering, enum niche-packing, passing large
//! aggregates by `const*` — all change the emitted C for programs that already exist.
//! That would invalidate every golden file, the concatenated build, the bootstrap seed
//! and every attested hash *at once*. So layout lands opt-in, and the first increment
//! changes **no emission at all**: it only tells you what the layout already is.
//!
//! That ordering is not merely cautious. "Which of my types waste space?" is the
//! question a systems programmer actually asks first, and answering it needs none of
//! the risk. The reordering increments then have a report to justify themselves against.
//!
//! ## The model, and who the authority is
//! Jestyr compiles through C, so the **C compiler owns the real layout**. This module
//! reproduces the rules that compiler follows for the types Jestyr emits — sequential
//! fields at natural alignment, tail padding to the aggregate's alignment — under an
//! **LP64** target model (`size_of::<usize>() == 8`).
//!
//! It is therefore a *model*, and a model that silently disagreed with reality would be
//! worse than none. `layout_matches_c_sizeof` (a `c-oracle` test) generates a C program
//! that prints `sizeof`/`_Alignof`/`offsetof` for every corpus type and compares them
//! against these numbers, so the model is **verified against the compiler that decides**,
//! not merely asserted.
//!
//! ## What this unblocks
//! `@size_of`/`@align_of`/`@offset_of` are today **C-deferred** intrinsics: they lower
//! to `sizeof()`/`_Alignof()`/`offsetof()`, so the Jestyr compiler never learns the
//! numbers. Once they can be answered here they become comptime *values*, which closes
//! the gap `docs/ctfe-tiers.md` records against tier 3.

use std::fmt::Write;

use std::collections::HashSet;

use crate::ast::{Ast, Item, StructMember};
use crate::types::{Ty, TypeInfo, TypeKindG};

/// The target's pointer/`usize` width. Jestyr targets LP64 through its C backend; a
/// 32-bit target would change this one constant and nothing else in the module.
const PTR: u64 = 8;

/// A size/alignment pair — what every type reduces to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
}

impl Layout {
    fn new(size: u64, align: u64) -> Layout {
        Layout { size, align }
    }
    /// A scalar whose size and alignment are equal — the common case.
    fn scalar(n: u64) -> Layout {
        Layout { size: n, align: n }
    }
}

/// One field's placement inside its aggregate.
#[derive(Clone, Debug)]
pub struct FieldLayout {
    pub name: String,
    pub ty: String,
    pub offset: u64,
    pub size: u64,
    pub align: u64,
    /// Padding inserted *before* this field to satisfy its alignment.
    pub pad_before: u64,
}

/// A declared type's full layout.
#[derive(Clone, Debug)]
pub struct TypeLayout {
    pub name: String,
    /// `struct` / `record` / `enum` / `distinct` — the declaration form.
    pub kind: &'static str,
    pub size: u64,
    pub align: u64,
    /// Bytes lost to padding: inter-field gaps plus tail padding. The number the
    /// reordering increment exists to reduce.
    pub waste: u64,
    pub fields: Vec<FieldLayout>,
    /// Set when some component's layout is not knowable here (an unresolved generic,
    /// an opaque type). The whole record is then advisory, and says so.
    pub incomplete: bool,
}

/// Round `n` up to a multiple of `align`.
fn align_to(n: u64, align: u64) -> u64 {
    if align == 0 {
        return n;
    }
    n.div_ceil(align) * align
}

/// The layout of a primitive, or `None` if the name is not one.
fn prim_layout(name: &str) -> Option<Layout> {
    Some(match name {
        "i8" | "u8" | "bool" => Layout::scalar(1),
        "i16" | "u16" => Layout::scalar(2),
        "i32" | "u32" | "f32" | "char" => Layout::scalar(4),
        "i64" | "u64" | "f64" | "isize" | "usize" => Layout::scalar(8),
        // `str` is a length-carrying view: `{ const char* ptr; size_t len; }`.
        "str" | "os_str" => Layout::new(2 * PTR, PTR),
        // `cstr` is a bare NUL-terminated pointer.
        "cstr" => Layout::scalar(PTR),
        // Owned/growable buffers: `{ ptr, len, cap }`.
        "String" | "Cow" => Layout::new(3 * PTR, PTR),
        // `{ JestyrStr* frags; size_t n; size_t cap; }`.
        "Builder" => Layout::new(3 * PTR, PTR),
        _ => return None,
    })
}

/// The layout of an arbitrary `Ty`. `None` when it cannot be known from the declared
/// shape alone — an unresolved generic parameter, or an opaque/erroneous type.
pub fn layout_of(info: &TypeInfo, ty: &Ty) -> Option<Layout> {
    Some(match ty {
        Ty::Unit => Layout::new(0, 1),
        Ty::Prim(p) => prim_layout(p)?,
        // Every pointer-ish thin handle is one machine word.
        Ty::Ptr { .. } | Ty::RegionRef(_) | Ty::Fn { .. } => Layout::scalar(PTR),
        // `{ T* ptr; size_t len; }` and `{ T* ptr; uint64_t gen; }` — two words each.
        Ty::Slice(_) | Ty::GenRef(_) => Layout::new(2 * PTR, PTR),
        // A value array lowers to `struct { T a[N]; }`: N elements, the element's
        // alignment, and no padding beyond what the element already carries.
        Ty::Array { elem, len } => {
            let e = layout_of(info, elem)?;
            Layout::new(e.size * (*len as u64), e.align)
        }
        Ty::Named(i) => named_layout(info, *i)?,
        // A `T !E` result carries an ok-value, a tag and an error code.
        Ty::Result(inner) => {
            let ok = layout_of(info, inner)?;
            aggregate(&[Layout::scalar(1), ok, Layout::scalar(4)]).0
        }
        // Generic instances, opaque names, type-valued and task types: not knowable
        // from the declaration alone. Reported as incomplete rather than guessed.
        Ty::Opaque(_)
        | Ty::GenStruct { .. }
        | Ty::GenEnum { .. }
        | Ty::Task(_)
        | Ty::TypeKw
        | Ty::Unknown
        | Ty::Error => return None,
    })
}

/// The layout of a declared type by table index.
fn named_layout(info: &TypeInfo, idx: usize) -> Option<Layout> {
    let decl = info.table.types.get(idx)?;
    match &decl.kind {
        TypeKindG::Struct { fields } => {
            let mut ls = Vec::with_capacity(fields.len());
            for (_, t) in fields {
                ls.push(layout_of(info, t)?);
            }
            Some(aggregate(&ls).0)
        }
        // A tagged enum is `{ tag; union { payloads } }`: the union takes the widest
        // payload and the strictest alignment, and the whole thing is padded to that.
        TypeKindG::Enum { variants } => {
            let mut payload = Layout::new(0, 1);
            for (_, ts) in variants {
                let mut ls = Vec::with_capacity(ts.len());
                for t in ts {
                    ls.push(layout_of(info, t)?);
                }
                let (l, _) = aggregate(&ls);
                payload.size = payload.size.max(l.size);
                payload.align = payload.align.max(l.align);
            }
            Some(aggregate(&[Layout::scalar(4), payload]).0)
        }
        // `distinct` is a zero-cost nominal wrapper: the base's layout exactly.
        TypeKindG::Distinct { base } => layout_of(info, base),
    }
}

/// Lay out fields sequentially at natural alignment — the C rule. Returns the whole
/// aggregate's layout and each field's offset.
fn aggregate(fields: &[Layout]) -> (Layout, Vec<u64>) {
    let mut offset = 0u64;
    let mut align = 1u64;
    let mut offsets = Vec::with_capacity(fields.len());
    for f in fields {
        offset = align_to(offset, f.align);
        offsets.push(offset);
        offset += f.size;
        align = align.max(f.align);
    }
    // Tail padding: an aggregate's size is a multiple of its alignment, so that
    // `arr[i]` stays aligned for every `i`.
    (Layout::new(align_to(offset, align), align), offsets)
}

/// Names of structs declaring at least one **bit-field** (`flags: u8 : 3`).
///
/// Bit-field packing is *implementation-defined* in C — allocation order within a
/// storage unit, whether a field may straddle one, and the unit's size are all the
/// compiler's choice. A model therefore cannot be authoritative about them, and a
/// confident wrong number is worse than an admitted gap: this pass marks such a struct
/// **incomplete** rather than reporting the unpacked size it would otherwise compute
/// (`struct Packed { a: u8 : 1, b: u8 : 1, mode: u8 : 3, rest: u8 : 3 }` really occupies
/// one byte, not four).
fn bitfield_types(ast: &Ast) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &ast.items {
        if let Item::Struct { name, body, .. } = item {
            let has_bits = body.members.iter().any(
                |m| matches!(m, StructMember::Field { bits: Some(_), .. }),
            );
            if has_bits {
                out.insert(name.name.clone());
            }
        }
    }
    out
}

/// Compute a layout record for every declared type, in declaration order.
///
/// Takes the AST as well as the checked table because two things layout depends on are
/// syntax, not type: bit-field widths, and (later) opt-in layout attributes.
pub fn compute(ast: &Ast, info: &TypeInfo) -> Vec<TypeLayout> {
    let bitfields = bitfield_types(ast);
    let mut out = Vec::new();
    for (i, decl) in info.table.types.iter().enumerate() {
        // A generic template has no layout until it is instantiated.
        if !decl.type_params.is_empty() {
            continue;
        }
        let kind = match &decl.kind {
            TypeKindG::Struct { .. } if decl.is_record => "record",
            TypeKindG::Struct { .. } => "struct",
            TypeKindG::Enum { .. } => "enum",
            TypeKindG::Distinct { .. } => "distinct",
        };
        let mut fields = Vec::new();
        // A bit-field struct is unmodellable here (see `bitfield_types`), so its record
        // is advisory from the start rather than after the fact.
        let mut incomplete = bitfields.contains(&decl.name);

        if let TypeKindG::Struct { fields: fs } = &decl.kind {
            let mut ls = Vec::with_capacity(fs.len());
            for (_, t) in fs {
                match layout_of(info, t) {
                    Some(l) => ls.push(l),
                    None => {
                        incomplete = true;
                        ls.push(Layout::new(0, 1));
                    }
                }
            }
            let (_, offsets) = aggregate(&ls);
            let mut prev_end = 0u64;
            for (k, (name, t)) in fs.iter().enumerate() {
                fields.push(FieldLayout {
                    name: name.clone(),
                    ty: t.display(&info.table),
                    offset: offsets[k],
                    size: ls[k].size,
                    align: ls[k].align,
                    pad_before: offsets[k] - prev_end,
                });
                prev_end = offsets[k] + ls[k].size;
            }
        }

        let (size, align) = match named_layout(info, i) {
            Some(l) => (l.size, l.align),
            None => {
                incomplete = true;
                (0, 1)
            }
        };
        // Waste is everything that is not a field byte — the inter-field gaps and the
        // tail padding together. Only a struct has fields to compare against: a
        // `distinct` is by definition its base (nothing is lost), and an enum's slack
        // belongs to the union rather than to any one variant, so neither reports a
        // number here rather than reporting a misleading one.
        let waste = if matches!(decl.kind, TypeKindG::Struct { .. }) {
            let used: u64 = fields.iter().map(|f| f.size).sum();
            size.saturating_sub(used)
        } else {
            0
        };

        out.push(TypeLayout {
            name: decl.name.clone(),
            kind,
            size,
            align,
            waste,
            fields,
            incomplete,
        });
    }
    out
}

/// Render the report `jestyrc layout` prints. Deterministic and diffable: declaration
/// order, one line per type and per field, so it can be pinned in CI like every other
/// artifact this compiler produces.
pub fn render(layouts: &[TypeLayout]) -> String {
    let mut out = String::new();
    out.push_str("layout v1\n");
    out.push_str(&format!("types {}\n", layouts.len()));
    for t in layouts {
        let _ = write!(out, "{} {} size {} align {} waste {}", t.kind, t.name, t.size, t.align, t.waste);
        if t.incomplete {
            // Said out loud rather than silently approximated: a record whose
            // components are generic cannot be trusted as a number.
            out.push_str(" (incomplete)");
        }
        out.push('\n');
        for f in &t.fields {
            if f.pad_before > 0 {
                let _ = writeln!(out, "  pad {}", f.pad_before);
            }
            let _ = writeln!(
                out,
                "  field {} offset {} size {} align {} : {}",
                f.name, f.offset, f.size, f.align, f.ty
            );
        }
        // Tail padding is reported explicitly: it is the part people forget, and the
        // part reordering cannot remove (only a smaller alignment can).
        if !t.fields.is_empty() {
            let last = t.fields.last().expect("non-empty");
            let tail = t.size.saturating_sub(last.offset + last.size);
            if tail > 0 {
                let _ = writeln!(out, "  tail-pad {tail}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn layouts_of(src: &str) -> Vec<TypeLayout> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, d) = Parser::new(src, tokens).parse();
        assert!(d.iter().all(|x| !x.is_error()), "fixture must parse: {d:?}");
        let (info, _) = crate::typeck::check(&ast);
        compute(&ast, &info)
    }

    fn by_name(ls: &[TypeLayout], n: &str) -> TypeLayout {
        ls.iter().find(|l| l.name == n).unwrap_or_else(|| panic!("no type {n}")).clone()
    }

    #[test]
    fn packs_a_struct_by_the_c_rules() {
        // The classic: a byte, a word, a byte. Sequential placement pads before `b`
        // and again at the tail, so 1+8+1 bytes of data occupy 24.
        let ls = layouts_of("struct S { a: u8, b: u64, c: u8 }\n");
        let s = by_name(&ls, "S");
        assert_eq!((s.size, s.align), (24, 8));
        assert_eq!(s.fields[0].offset, 0);
        assert_eq!(s.fields[1].offset, 8);
        assert_eq!(s.fields[1].pad_before, 7);
        assert_eq!(s.fields[2].offset, 16);
        assert_eq!(s.waste, 24 - 10);
    }

    #[test]
    fn a_reordered_field_list_is_already_tighter() {
        // The same three fields, largest first: no interior padding at all. This is
        // the win the opt-in reordering increment will make automatic — the analysis
        // pass exists to show it is worth taking.
        let ls = layouts_of("struct T { b: u64, a: u8, c: u8 }\n");
        let t = by_name(&ls, "T");
        assert_eq!((t.size, t.align), (16, 8));
        assert_eq!(t.waste, 6);
    }

    #[test]
    fn primitives_and_fat_pointers_have_their_c_widths() {
        let ls = layouts_of("struct P { a: bool, b: i16, c: f64, d: str }\n");
        let p = by_name(&ls, "P");
        assert_eq!(p.fields[0].size, 1);
        assert_eq!(p.fields[1].size, 2);
        assert_eq!(p.fields[2].size, 8);
        // `str` is `{ ptr, len }` — a view, not a byte.
        assert_eq!((p.fields[3].size, p.fields[3].align), (16, 8));
    }

    #[test]
    fn an_array_field_multiplies_its_element() {
        let ls = layouts_of("struct A { xs: [4]i32, y: u8 }\n");
        let a = by_name(&ls, "A");
        assert_eq!((a.fields[0].size, a.fields[0].align), (16, 4));
        assert_eq!((a.size, a.align), (20, 4));
    }

    #[test]
    fn a_distinct_type_costs_exactly_its_base() {
        let ls = layouts_of("distinct UserId = u64\nstruct W { id: UserId }\n");
        let u = by_name(&ls, "UserId");
        assert_eq!((u.size, u.align), (8, 8));
        assert_eq!(u.waste, 0, "a zero-cost wrapper must waste nothing");
        assert_eq!(by_name(&ls, "W").size, 8);
    }

    #[test]
    fn an_enum_is_a_tag_plus_its_widest_payload() {
        // Enum payloads carry named fields (`some(v: i64)`), like a struct variant.
        let ls = layouts_of("enum E { none, some(v: i64), pair(a: i32, b: i32) }\n");
        let e = by_name(&ls, "E");
        // tag(4) + pad(4) + widest payload(8) = 16, aligned to 8.
        assert_eq!((e.size, e.align), (16, 8));
    }

    #[test]
    fn a_generic_template_has_no_layout_and_is_skipped() {
        // A template is not a type until instantiated; reporting a number for it
        // would be inventing one.
        let ls = layouts_of("enum Option(T) { none, some(v: T) }\nstruct S { a: u8 }\n");
        assert!(ls.iter().all(|l| l.name != "Option"), "a template must not be reported");
        assert!(ls.iter().any(|l| l.name == "S"));
    }

    #[test]
    fn an_unknowable_component_is_flagged_not_guessed() {
        let ls = layouts_of("struct H { v: Vec }\n");
        let h = by_name(&ls, "H");
        assert!(h.incomplete, "an opaque component must mark the record incomplete");
    }

    /// Bit-field packing is implementation-defined in C, so this pass refuses to state
    /// a number rather than stating a wrong one. Without the check it would report the
    /// UNPACKED size (four u8 fields → 4 bytes) for a struct that really occupies one.
    #[test]
    fn a_bitfield_struct_is_admitted_unmodellable_not_guessed() {
        let ls = layouts_of("struct Packed { a: u8 : 1, b: u8 : 1, mode: u8 : 3, rest: u8 : 3 }\n");
        let p = by_name(&ls, "Packed");
        assert!(p.incomplete, "a bit-field struct must not be reported as a known layout");
        assert!(render(&ls).contains("(incomplete)"), "the report must say so out loud");
        // A struct with the same fields and no bit widths IS modellable — so the flag
        // tracks the bit-fields, not merely the field types.
        let plain = layouts_of("struct Wide { a: u8, b: u8, mode: u8, rest: u8 }\n");
        assert!(!by_name(&plain, "Wide").incomplete);
        assert_eq!(by_name(&plain, "Wide").size, 4);
    }

    #[test]
    fn the_report_is_deterministic() {
        let src = "struct S { a: u8, b: u64 }\nenum E { x, y(v: i32) }\ndistinct D = u16\n";
        let first = render(&layouts_of(src));
        for _ in 0..5 {
            assert_eq!(render(&layouts_of(src)), first);
        }
        assert!(first.starts_with("layout v1\n"));
    }
}
