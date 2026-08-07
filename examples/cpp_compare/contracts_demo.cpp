// Design-by-contract style in C++17: close equivalent to contracts_demo.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -o "$env:TEMP\contracts_demo_cpp.exe" examples\cpp_compare\contracts_demo.cpp

#include <cassert>
#include <iostream>

static int clamp(int x, int lo, int hi) {
    assert(lo <= hi);

    int result = x;
    if (x < lo) {
        result = lo;
    } else if (x > hi) {
        result = hi;
    }

    assert(result >= lo);
    assert(result <= hi);
    return result;
}

int main() {
    std::cout << clamp(12, 0, 10) << '\n';
    std::cout << clamp(-4, 0, 10) << '\n';
    std::cout << clamp(7, 0, 10) << '\n';
}
