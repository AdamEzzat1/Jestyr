// heavy_wordcount.cpp — the C++17 twin of heavy_wordcount.jtr: same LCG stream,
// same vocabulary, std::unordered_map instead of Jestyr's std/strmap.
//
// Build:  g++ -O2 -std=c++17 heavy_wordcount.cpp -o heavy_wordcount
#include <cstdio>
#include <cstdint>
#include <string>
#include <unordered_map>

static const std::int64_t TOTAL = 10000000;

static const char* WORDS[16] = {
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
    "iota", "kappa", "lambda", "mu", "nu", "xi", "omicron", "pi",
};

int main() {
    std::unordered_map<std::string, std::int64_t> m;

    std::int64_t state = 20260808;
    for (std::int64_t n = 0; n < TOTAL; ++n) {
        state = (state * 48271) % 2147483647;
        m[WORDS[state % 16]] += 1;
    }

    std::printf("%lld\n", (long long)m.size());
    std::printf("%lld\n", (long long)m["alpha"]);
    std::printf("%lld\n", (long long)m["theta"]);
    std::printf("%lld\n", (long long)m["pi"]);
    return 0;
}
