// Parallel reduction in C++17: close equivalent to parallel_reduce.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -pthread -o "$env:TEMP\parallel_reduce_cpp.exe" examples/cpp_compare/parallel_reduce.cpp
//
// This is written straight, not as a strawman: manual chunking into disjoint ranges
// with per-thread partials and a serial combine, which is what the Jestyr `par for`
// desugars to as well. Two differences are worth noticing, and neither is about
// syntax.
//
// 1. Determinism here is an accident of the operator, not a checked property. These
//    reductions are schedule-independent because integer `+` and `max` are
//    associative. Change `std::int64_t` to `double` and the same code silently
//    becomes schedule-dependent -- the answer starts varying with kWorkers -- and
//    nothing in the language, the library, or `-Wall` will say a word. The Jestyr
//    pair cannot express that program: `par for` accepts only reductions whose
//    associativity has been declared and checked.
//
// 2. There is no way to state, let alone verify, that par_sum_sq has O(log n) span
//    while serial_sum_sq has O(n). If someone later replaces the threaded body with
//    the serial loop, this file still compiles and still passes its tests; it just
//    quietly stops being parallel. The Jestyr pair declares `@span(log)` and the
//    compiler rejects that edit.
//
// std::reduce with a parallel execution policy would shorten this, but it is not the
// honest comparison: it requires a TBB-backed libstdc++ that is not present on many
// toolchains (including the MinGW GCC 8 used to check this suite), and it would move
// the same unchecked associativity precondition into a template parameter.

#include <algorithm>
#include <cstdint>
#include <iostream>
#include <thread>
#include <vector>

static const int kWorkers = 4;

// Run `body(lo, hi)` over kWorkers disjoint chunks and collect one partial each.
template <class F>
static std::vector<std::int64_t> map_chunks(std::size_t n, F body) {
    std::vector<std::int64_t> partial(kWorkers, 0);
    std::vector<std::thread> threads;
    threads.reserve(kWorkers);
    const std::size_t chunk = (n + kWorkers - 1) / kWorkers;
    for (int w = 0; w < kWorkers; ++w) {
        threads.emplace_back([&, w] {
            const std::size_t lo = std::min(n, static_cast<std::size_t>(w) * chunk);
            const std::size_t hi = std::min(n, lo + chunk);
            partial[w] = body(lo, hi);
        });
    }
    for (auto& t : threads) {
        t.join();
    }
    return partial;
}

static std::int64_t par_sum_sq(const std::vector<std::int64_t>& xs) {
    const auto partial = map_chunks(xs.size(), [&](std::size_t lo, std::size_t hi) {
        std::int64_t acc = 0;
        for (std::size_t i = lo; i < hi; ++i) {
            acc += xs[i] * xs[i];
        }
        return acc;
    });
    std::int64_t total = 0;
    for (const auto p : partial) {
        total += p;
    }
    return total;
}

static std::int64_t par_max(const std::vector<std::int64_t>& xs) {
    const auto partial = map_chunks(xs.size(), [&](std::size_t lo, std::size_t hi) {
        std::int64_t best = INT64_MIN;
        for (std::size_t i = lo; i < hi; ++i) {
            best = std::max(best, xs[i]);
        }
        return best;
    });
    std::int64_t best = INT64_MIN;
    for (const auto p : partial) {
        best = std::max(best, p);
    }
    return best;
}

static std::int64_t serial_sum_sq(const std::vector<std::int64_t>& xs) {
    std::int64_t acc = 0;
    for (const auto x : xs) {
        acc += x * x;
    }
    return acc;
}

int main() {
    constexpr std::size_t n = 1000;
    std::vector<std::int64_t> xs(n);
    for (std::size_t i = 0; i < n; ++i) {
        xs[i] = static_cast<std::int64_t>(i) + 1;
    }

    std::cout << par_sum_sq(xs) << '\n';                        // 333833500
    std::cout << (par_sum_sq(xs) == serial_sum_sq(xs)) << '\n'; // 1
    std::cout << par_max(xs) << '\n';                           // 1000
}
