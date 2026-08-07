// C++17 analogue for static_rejections.jtr.
//
// This compiles, but it relies on programmer discipline: the switch has a
// fallback value instead of an exhaustiveness proof, `bad_region_escape` returns a
// dangling pointer into a destroyed local buffer, and `schedule_dependent_sum` is a
// parallel float reduction whose result changes with the worker count.
//
// Only the first of the three draws a diagnostic, and only if you opt into `-Wall`:
//   g++ -O2 -std=c++17 -pthread -Wall -o "$env:TEMP\static_rejections_cpp.exe" examples/cpp_compare/static_rejections.cpp
//
// The dangling return is silent because it goes through std::array::data(), which is
// enough indirection to defeat -Wreturn-local-addr. The float reduction is silent
// because there is nothing to warn about: it is a perfectly legal program that
// happens to have no single answer.

#include <array>
#include <cstddef>
#include <thread>
#include <vector>

enum class Color { red, green, blue };

static int paint(Color c) {
    switch (c) {
    case Color::red: return 0;
    case Color::green: return 1;
    }
    return -1;
}

static const char* bad_region_escape() {
    std::array<char, 8> local = {'J', 'e', 's', 't', 'y', 'r', '!', '\0'};
    return local.data();
}

// The analogue of Jestyr's rejected `par for`: a parallel sum over a non-associative
// operator. Floating-point `+` does not reassociate, so regrouping the stream into
// `workers` chunks changes which roundings happen. Call this with workers = 2 and
// again with workers = 4 and you can get different answers from the same input.
// It compiles clean at any warning level. Jestyr will not accept the equivalent.
static double schedule_dependent_sum(const std::vector<double>& xs, int workers) {
    std::vector<double> partial(static_cast<std::size_t>(workers), 0.0);
    std::vector<std::thread> threads;
    const std::size_t n = xs.size();
    const std::size_t chunk = (n + workers - 1) / workers;
    for (int w = 0; w < workers; ++w) {
        threads.emplace_back([&, w] {
            const std::size_t lo = std::min(n, static_cast<std::size_t>(w) * chunk);
            const std::size_t hi = std::min(n, lo + chunk);
            double acc = 0.0;
            for (std::size_t i = lo; i < hi; ++i) {
                acc += xs[i];
            }
            partial[static_cast<std::size_t>(w)] = acc;
        });
    }
    for (auto& t : threads) {
        t.join();
    }
    double total = 0.0;
    for (const auto p : partial) {
        total += p;
    }
    return total;
}

int main() {
    const std::vector<double> xs = {1.0, 1e100, 1.0, -1e100};
    const bool same = schedule_dependent_sum(xs, 2) == schedule_dependent_sum(xs, 4);
    (void)same;  // may be true or false; the point is that nothing checked it
    return paint(Color::blue) == -1 && bad_region_escape() != nullptr ? 0 : 1;
}
