// heavy_matmul.cpp — the C++17 twin of heavy_matmul.jtr: same ikj order, same
// integer-valued inputs, same exact checksum.
//
// Build:  g++ -O2 -std=c++17 heavy_matmul.cpp -o heavy_matmul
#include <cstdio>
#include <cstdint>
#include <vector>

static const std::size_t N = 768;
static const std::int64_t MOD = 1000000007;

static void fill(std::vector<double>& p, std::int64_t salt) {
    for (std::size_t i = 0; i < p.size(); ++i) {
        std::int64_t x = ((std::int64_t)i * 1103515245 + salt) % 1000;
        p[i] = (double)x;
    }
}

int main() {
    const std::size_t nn = N * N;
    std::vector<double> a(nn), b(nn), c(nn, 0.0);
    fill(a, 12345);
    fill(b, 54321);

    for (std::size_t i = 0; i < N; ++i) {
        for (std::size_t k = 0; k < N; ++k) {
            double aik = a[i * N + k];
            for (std::size_t j = 0; j < N; ++j) {
                c[i * N + j] += aik * b[k * N + j];
            }
        }
    }

    double sum = 0.0;
    for (std::size_t i = 0; i < nn; ++i) sum += c[i];
    std::printf("%lld\n", (long long)(((std::int64_t)sum) % MOD));
    return 0;
}
