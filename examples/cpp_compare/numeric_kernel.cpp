// Numeric kernel in C++17: close equivalent to numeric_kernel.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -ffp-contract=off -fno-fast-math -o "$env:TEMP\numeric_kernel_cpp.exe" examples/cpp_compare/numeric_kernel.cpp
//
// Note: `xs[i]` on std::vector is unchecked. The Jestyr pair's `xs[i]` lowers to
// a bounds-checked access, so the two programs are not doing identical work.

#include <cstdint>
#include <iostream>
#include <vector>

static void fill(std::vector<std::int64_t>& xs) {
    for (std::size_t i = 0; i < xs.size(); ++i) {
        const auto x = static_cast<std::int64_t>(i);
        xs[i] = ((x * 1664525) + 1013904223) % 1000003;
    }
}

static void stencil(const std::vector<std::int64_t>& xs, std::vector<std::int64_t>& ys) {
    ys[0] = xs[0];
    ys[xs.size() - 1] = xs[xs.size() - 1];
    for (std::size_t i = 1; i + 1 < xs.size(); ++i) {
        ys[i] = (xs[i - 1] * 3) + (xs[i] * 5) - (xs[i + 1] * 2);
    }
}

static std::int64_t checksum(const std::vector<std::int64_t>& xs) {
    std::int64_t total = 0;
    for (auto x : xs) {
        total = (total + x) % 2147483647;
    }
    return total;
}

int main() {
    constexpr std::size_t n = 200000;
    std::vector<std::int64_t> xs(n);
    std::vector<std::int64_t> ys(n);

    fill(xs);
    stencil(xs, ys);
    std::cout << checksum(ys) << '\n';
}
