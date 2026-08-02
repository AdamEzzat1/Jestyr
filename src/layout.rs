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

use crate::ast::{Ast, Attribute, ExprKind, Item, StructMember};
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
    /// Set when `@layout(auto)` applies, so `fields` is in **emission** order rather
    /// than declaration order. Rendered, because a reader comparing this report against
    /// their source needs to know why the field list is shuffled.
    pub reordered: bool,
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

/// The names of the structs that opted into `@layout(auto)`.
///
/// Collected once and threaded through the whole size computation, because reordering
/// is **not** a local property: a reordered struct is usually smaller, so a struct that
/// embeds one must see the smaller number or every offset after it would be wrong. This
/// is the reason `layout_of` takes the set rather than each caller checking attributes
/// at the top level.
pub fn auto_types(ast: &Ast, info: &TypeInfo) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &ast.items {
        if let Item::Struct { name, attrs, .. } = item {
            if field_order(ast, info, &name.name, attrs).is_some() {
                out.insert(name.name.clone());
            }
        }
    }
    out
}

/// The layout of an arbitrary `Ty`. `None` when it cannot be known from the declared
/// shape alone — an unresolved generic parameter, or an opaque/erroneous type.
///
/// `auto` names the structs whose fields are reordered (see [`auto_types`]); pass an
/// empty set for the plain declaration-order model.
pub fn layout_of(info: &TypeInfo, auto: &HashSet<String>, ty: &Ty) -> Option<Layout> {
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
            let e = layout_of(info, auto, elem)?;
            Layout::new(e.size * (*len as u64), e.align)
        }
        Ty::Named(i) => named_layout(info, auto, *i)?,
        // A `T !E` result carries an ok-value, a tag and an error code.
        Ty::Result(inner) => {
            let ok = layout_of(info, auto, inner)?;
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
fn named_layout(info: &TypeInfo, auto: &HashSet<String>, idx: usize) -> Option<Layout> {
    let decl = info.table.types.get(idx)?;
    match &decl.kind {
        TypeKindG::Struct { fields } => {
            let mut ls = Vec::with_capacity(fields.len());
            for (_, t) in fields {
                ls.push(layout_of(info, auto, t)?);
            }
            // A reordered struct is laid out in the order the backend will emit it,
            // which is the only reason its size can differ from the declared one.
            if auto.contains(&decl.name) {
                let aligns: Vec<u64> = ls.iter().map(|l| l.align).collect();
                ls = auto_order(&aligns).into_iter().map(|k| ls[k]).collect();
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
                    ls.push(layout_of(info, auto, t)?);
                }
                let (l, _) = aggregate(&ls);
                payload.size = payload.size.max(l.size);
                payload.align = payload.align.max(l.align);
            }
            Some(aggregate(&[Layout::scalar(4), payload]).0)
        }
        // `distinct` is a zero-cost nominal wrapper: the base's layout exactly.
        TypeKindG::Distinct { base } => layout_of(info, auto, base),
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

// ── `@layout(auto)` — opt-in field reordering (increment L2) ────────────────────

/// Does this struct opt into automatic field ordering (`@layout(auto)`)?
///
/// The default — no attribute at all, or the explicit `@layout(c)` — is declaration
/// order, which is what every program emitted before this increment got and what every
/// program still gets unless it asks otherwise. That is the whole reason the feature is
/// an attribute: reordering unconditionally would rewrite the emitted C of every
/// existing program at once, invalidating the golden corpus, the concatenated build,
/// the bootstrap seed and every attested hash in a single commit.
pub fn wants_auto(ast: &Ast, attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| a.name == "layout" && layout_word(ast, a).as_deref() == Some("auto"))
}

/// The identifier argument of a `@layout(<word>)` attribute, if it has one.
///
/// Shared with the validator in `attrs.rs` so the vocabulary the compiler *accepts*
/// and the vocabulary it *acts on* cannot drift — the `at_ty` / `simd::classify` rule
/// that has kept the reflected, documented and attested type renderings in agreement.
pub fn layout_word(ast: &Ast, a: &Attribute) -> Option<&'static str> {
    // Attribute arguments are expressions; `@layout`'s is validated as a single bare
    // identifier, so this reads that one shape and yields nothing for anything else.
    let ExprKind::Name(n) = &ast.expr_at(*a.args.first()?).kind else {
        return None;
    };
    // Returned as a `&'static str` from the closed vocabulary rather than as the user's
    // own string: a caller then cannot compare against a spelling the validator would
    // have rejected.
    LAYOUT_WORDS.iter().find(|w| **w == n.name).copied()
}

/// The complete vocabulary of `@layout(<word>)`.
///
/// * `c` — the default. Fields are emitted in declaration order, which is what C
///   guarantees and what an FFI struct needs.
/// * `auto` — the compiler picks the order that minimises padding (see [`auto_order`]).
///
/// Closed on purpose. `@layout(packd)` used to validate clean and do nothing, because
/// the argument was checked for *being* an identifier and never for *which* one — and
/// an attribute that quietly means nothing reads exactly like a guarantee. The same
/// argument that makes `@simd` on a function with no `par for` an error.
pub const LAYOUT_WORDS: &[&str] = &["c", "auto"];

/// The order `@layout(auto)` emits a struct's fields in: **descending alignment**,
/// with declaration order breaking ties (a stable sort).
///
/// ## Why this is minimal, not merely tighter
/// Every layout this model produces satisfies `size % align == 0` — scalars are square,
/// an array is `n` elements of a type that already obeys the rule, and `aggregate`
/// tail-pads to the alignment. Alignments are powers of two. Together those give the
/// result: place the fields in non-increasing alignment and each field's offset is the
/// sum of the sizes before it, every one of which is a multiple of an alignment *at
/// least as strict* as this field's — so every offset is already aligned and **no
/// interior padding is inserted at all**. The total is then `align_to(Σ sizes, max
/// align)`, which no ordering can beat, since Σ sizes is a lower bound and the
/// aggregate must be a multiple of its alignment regardless.
///
/// So this is not a packing heuristic with a good average case; it is the optimum under
/// a stated invariant, and `auto_ordering_leaves_no_interior_padding` checks the
/// invariant rather than trusting it.
///
/// Ties break by declaration order because the ordering must be **deterministic and
/// stable**: it feeds the emitted C, which feeds the attest hash. A sort that depended
/// on hash iteration order would make a build unreproducible.
pub fn auto_order(aligns: &[u64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..aligns.len()).collect();
    // `sort_by_key` is stable, so equal alignments keep their declaration order.
    order.sort_by_key(|&i| std::cmp::Reverse(aligns[i]));
    order
}

/// The field emission order for a declared struct: `Some(perm)` when it asked for
/// `@layout(auto)` and the request can be honoured, `None` for declaration order.
///
/// Returns `None` — silently, because the *loud* refusals are the validator's job in
/// `attrs.rs` — when some field's layout is not knowable here. Reordering by an
/// alignment the compiler had to guess would be worse than not reordering: the model
/// would then disagree with the C compiler about the very offsets it claims to improve.
pub fn field_order(
    ast: &Ast,
    info: &TypeInfo,
    name: &str,
    attrs: &[Attribute],
) -> Option<Vec<usize>> {
    if !wants_auto(ast, attrs) {
        return None;
    }
    let idx = *info.table.type_index.get(name)?;
    let TypeKindG::Struct { fields } = &info.table.types.get(idx)?.kind else {
        return None;
    };
    // The ordering needs only each field's ALIGNMENT, and alignment is
    // order-invariant — an aggregate's alignment is the max over its components, which
    // no permutation changes. So this may ask for layouts under the empty auto-set
    // without first knowing which structs are reordered, which is what breaks the
    // circularity between `auto_types` and `field_order`.
    let none = HashSet::new();
    let mut aligns = Vec::with_capacity(fields.len());
    for (_, t) in fields {
        aligns.push(layout_of(info, &none, t)?.align);
    }
    Some(auto_order(&aligns))
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
/// syntax, not type: bit-field widths, and the opt-in `@layout(auto)` attribute.
///
/// A reordered struct's **fields are listed in emission order**, not declaration order.
/// The report exists to describe the bytes the backend actually produces, and a report
/// that described the source order would be describing a struct that no longer exists.
pub fn compute(ast: &Ast, info: &TypeInfo) -> Vec<TypeLayout> {
    let bitfields = bitfield_types(ast);
    let auto = auto_types(ast, info);
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

        let reordered = auto.contains(&decl.name);

        if let TypeKindG::Struct { fields: fs } = &decl.kind {
            let mut ls = Vec::with_capacity(fs.len());
            for (_, t) in fs {
                match layout_of(info, &auto, t) {
                    Some(l) => ls.push(l),
                    None => {
                        incomplete = true;
                        ls.push(Layout::new(0, 1));
                    }
                }
            }
            // Emission order: the permutation for a reordered struct, the identity for
            // every other one. Offsets are then computed over that order, so the numbers
            // below describe the emitted C in both cases.
            let order: Vec<usize> = if reordered {
                auto_order(&ls.iter().map(|l| l.align).collect::<Vec<_>>())
            } else {
                (0..fs.len()).collect()
            };
            let placed: Vec<Layout> = order.iter().map(|&k| ls[k]).collect();
            let (_, offsets) = aggregate(&placed);
            let mut prev_end = 0u64;
            for (slot, &k) in order.iter().enumerate() {
                let (name, t) = &fs[k];
                fields.push(FieldLayout {
                    name: name.clone(),
                    ty: t.display(&info.table),
                    offset: offsets[slot],
                    size: ls[k].size,
                    align: ls[k].align,
                    pad_before: offsets[slot] - prev_end,
                });
                prev_end = offsets[slot] + ls[k].size;
            }
        }

        let (size, align) = match named_layout(info, &auto, i) {
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
            reordered,
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
        if t.reordered {
            // Said out loud: the field list below is emission order, so it will not
            // match the source, and the numbers describe the shuffled struct.
            out.push_str(" (reordered)");
        }
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

    // ── `@layout(auto)` (increment L2) ──────────────────────────────────────────

    /// **The invariant the optimality argument rests on.** `auto_order` claims to be
    /// minimal, not merely tighter, and the proof needs every layout to satisfy
    /// `size % align == 0` — otherwise a field could start at an unaligned offset even
    /// in descending-alignment order and interior padding would reappear.
    ///
    /// Checked over the shapes the model has rules for rather than argued, because the
    /// claim silently stops holding the moment a future type breaks the pattern.
    #[test]
    fn every_layout_size_is_a_multiple_of_its_alignment() {
        let ls = layouts_of(
            "struct S { a: u8, b: u64, c: u8 }\n\
             struct N { s: S, f: f32 }\n\
             struct A { xs: [3]i32, t: str }\n\
             enum E { none, some(v: i64) }\n\
             distinct D = u16\n\
             struct W { d: D, e: E, p: *mut i32 }\n",
        );
        for t in &ls {
            assert_eq!(t.size % t.align, 0, "{} is {} bytes at align {}", t.name, t.size, t.align);
            for f in &t.fields {
                assert_eq!(f.size % f.align, 0, "field {}.{} breaks the invariant", t.name, f.name);
            }
        }
    }

    /// Descending alignment leaves **no interior padding at all** — the whole claim.
    /// Every byte of slack is tail padding, which no ordering can remove (only a
    /// smaller alignment can), so `waste == align_to(Σ sizes, align) - Σ sizes`.
    #[test]
    fn auto_ordering_leaves_no_interior_padding() {
        let ls = layouts_of(
            "@layout(auto) struct T { a: u8, b: u64, c: u8, d: i32, e: u16 }\n",
        );
        let t = by_name(&ls, "T");
        assert!(t.reordered);
        assert!(
            t.fields.iter().all(|f| f.pad_before == 0),
            "descending alignment must insert no interior padding: {:?}",
            t.fields
        );
        let used: u64 = t.fields.iter().map(|f| f.size).sum();
        assert_eq!(t.size, align_to(used, t.align), "only tail padding may remain");
        // Declaration order would have cost more — the point of taking the option.
        let plain = layouts_of("struct T { a: u8, b: u64, c: u8, d: i32, e: u16 }\n");
        assert!(by_name(&plain, "T").size > t.size, "reordering must actually save bytes");
    }

    /// Ties keep declaration order, and the whole ordering is stable across runs. The
    /// emitted C feeds the attest hash, so an ordering that depended on hash iteration
    /// order would make a build unreproducible.
    #[test]
    fn auto_ordering_is_stable_and_deterministic() {
        assert_eq!(auto_order(&[1, 8, 1, 4, 1]), vec![1, 3, 0, 2, 4]);
        // Four fields of equal alignment: the identity, not an arbitrary shuffle.
        assert_eq!(auto_order(&[4, 4, 4, 4]), vec![0, 1, 2, 3]);
        let src = "@layout(auto) struct S { a: u8, b: u64, c: u8, d: u64 }\n";
        let first = render(&layouts_of(src));
        for _ in 0..5 {
            assert_eq!(render(&layouts_of(src)), first);
        }
        assert!(first.contains("(reordered)"), "the report must say the order changed");
    }

    /// Reordering is **not** a local property: a smaller inner struct moves every
    /// offset after it in the outer one. If the model failed to propagate, the report
    /// (and later the comptime `@offset_of`) would disagree with the emitted C.
    #[test]
    fn reordering_propagates_into_an_embedding_struct() {
        let src = "@layout(auto) struct Inner { a: u8, b: u64, c: u8 }\n\
                   struct Outer { i: Inner, tag: u8 }\n";
        let ls = layouts_of(src);
        // Inner: 8 + 1 + 1 → 16 (vs 24 in declaration order).
        assert_eq!(by_name(&ls, "Inner").size, 16);
        // Outer must see 16, not the 24 it would have seen without propagation.
        let o = by_name(&ls, "Outer");
        assert_eq!(o.fields[0].size, 16, "the embedding struct must see the reordered size");
        assert_eq!(o.size, 24);
        // Outer itself is untouched: the attribute is per-struct, not contagious.
        assert!(!o.reordered);
    }

    /// The default is unchanged, and `@layout(c)` is exactly the default said out loud.
    #[test]
    fn the_c_policy_is_the_untouched_default() {
        let plain = layouts_of("struct S { a: u8, b: u64, c: u8 }\n");
        let explicit = layouts_of("@layout(c) struct S { a: u8, b: u64, c: u8 }\n");
        assert_eq!(render(&plain), render(&explicit));
        assert!(!by_name(&explicit, "S").reordered);
        assert!(!render(&plain).contains("(reordered)"));
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
