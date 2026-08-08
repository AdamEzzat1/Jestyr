// heavy_parsum.cpp — the C++17 twin of heavy_parsum.jtr: hand-rolled chunking,
// four std::threads, per-thread partials, explicit join, then the serial check.
// Everything Jestyr's one `par for … reduce` line does, spelled out — and the
// determinism here is by CAREFUL CONVENTION (integer +, fixed chunk order);
// nothing checks it.
//
// Build:  g++ -O2 -std=c++17 -pthread heavy_parsum.cpp -o heavy_parsum
#include <cstdio>
#include <cstdint>
#include <vector>
#include <thread>

static const std::size_t N = 20000000;
static const std::int64_t MOD = 1000000007;
static const int WORKERS = 4;

int main() {
    std::vector<std::int64_t> a(N);
    for (std::size_t i = 0; i < N; ++i) a[i] = (std::int64_t)i % 1000;

    std::vector<std::int64_t> partial(WORKERS, 0);
    std::vector<std::thread> ts;
    for (int w = 0; w < WORKERS; ++w) {
        ts.emplace_back([&, w] {
            std::size_t lo = N * w / WORKERS, hi = N * (w + 1) / WORKERS;
            std::int64_t acc = 0;
            for (std::size_t i = lo; i < hi; ++i) acc += a[i] * a[i];
            partial[w] = acc;
        });
    }
    for (auto& t : ts) t.join();
    std::int64_t p = 0;
    for (int w = 0; w < WORKERS; ++w) p += partial[w];  // fixed combine order

    std::int64_t serial = 0;
    for (std::size_t i = 0; i < N; ++i) serial += a[i] * a[i];

    std::printf("%d\n", (int)(p % MOD));
    std::printf("%d\n", p == serial ? 1 : 0);
    return 0;
}
