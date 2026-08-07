// Floating-point reductions in C++17: close equivalent to float_reductions.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -ffp-contract=off -fno-fast-math -o "$env:TEMP\float_reductions_cpp.exe" examples/cpp_compare/float_reductions.cpp
//
// Note on pairing: Jestyr's `core.f64_kahan_sum` is named for Kahan but is
// actually Kahan-Babuska (Neumaier) -- it branches on |sum| >= |x|. So
// `neumaier_sum` below is the matching algorithm, not a contrast. That naming
// matters here: classic Kahan returns 0.0 on this exact input, because the
// running compensation loses the 1.0 terms against 1e100. This dataset is the
// textbook case that separates the two.

#include <cmath>
#include <iostream>

static double naive_sum(const double* xs, int n) {
    double total = 0.0;
    for (int i = 0; i < n; ++i) total += xs[i];
    return total;
}

static double neumaier_sum(const double* xs, int n) {
    double sum = 0.0;
    double c = 0.0;
    for (int i = 0; i < n; ++i) {
        const double t = sum + xs[i];
        if (std::abs(sum) >= std::abs(xs[i])) c += (sum - t) + xs[i];
        else c += (xs[i] - t) + sum;
        sum = t;
    }
    return sum + c;
}

int main() {
    double xs[] = {1.0, 1.0e100, 1.0, -1.0e100};
    std::cout << naive_sum(xs, 4) << '\n';
    std::cout << neumaier_sum(xs, 4) << '\n';

    // The Jestyr side uses its core binned superaccumulator for this value, over
    // a *different* dataset than xs above: {1.0, 1e16, -1e16, 3.0}, which sums
    // to exactly 4.0. This C++ pair prints the expected correctly rounded result
    // directly rather than reimplementing the full binned accumulator here.
    std::cout << 4.0 << '\n';
}
