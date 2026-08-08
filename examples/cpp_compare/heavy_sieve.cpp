// heavy_sieve.cpp — the C++17 twin of heavy_sieve.jtr: same algorithm, same
// traversal order, same checksum.
//
// Build:  g++ -O2 -std=c++17 heavy_sieve.cpp -o heavy_sieve
#include <cstdio>
#include <cstdint>
#include <vector>

static const std::size_t N = 50000000;
static const std::int64_t MOD = 1000000007;

int main() {
    std::vector<std::uint8_t> composite(N, 0);

    for (std::size_t i = 2; i * i < N; ++i) {
        if (composite[i] == 0) {
            for (std::size_t m = i * i; m < N; m += i) {
                composite[m] = 1;
            }
        }
    }

    std::int64_t count = 0, sum = 0;
    for (std::size_t i = 2; i < N; ++i) {
        if (composite[i] == 0) {
            count += 1;
            sum = (sum + (std::int64_t)i) % MOD;
        }
    }
    std::printf("%lld\n%lld\n", (long long)count, (long long)sum);
    return 0;
}
