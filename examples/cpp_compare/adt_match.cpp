// Algebraic data types in C++17: close equivalent to adt_match.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -o "$env:TEMP\adt_match_cpp.exe" examples\cpp_compare\adt_match.cpp

#include <cstdint>
#include <iostream>
#include <memory>
#include <variant>

struct Expr;

struct Lit {
    std::int32_t v;
};

struct Add {
    std::unique_ptr<Expr> l;
    std::unique_ptr<Expr> r;
};

struct Neg {
    std::unique_ptr<Expr> x;
};

struct Expr {
    std::variant<Lit, Add, Neg> node;
};

static std::int32_t eval(const Expr& e) {
    return std::visit(
        [](const auto& node) -> std::int32_t {
            using T = std::decay_t<decltype(node)>;
            if constexpr (std::is_same_v<T, Lit>) {
                return node.v;
            } else if constexpr (std::is_same_v<T, Add>) {
                return eval(*node.l) + eval(*node.r);
            } else {
                return -eval(*node.x);
            }
        },
        e.node);
}

int main() {
    auto a = std::make_unique<Expr>(Expr{Lit{40}});
    auto two = std::make_unique<Expr>(Expr{Lit{2}});
    auto b = std::make_unique<Expr>(Expr{Neg{std::move(two)}});
    Expr root{Add{std::move(a), std::move(b)}};

    std::cout << eval(root) << '\n';
    std::cout << sizeof(std::int32_t*) << '\n';
    std::cout << sizeof(std::int32_t*) << '\n';
}
