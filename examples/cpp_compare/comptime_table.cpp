// Compile-time table generation in C++17: close equivalent to comptime_table.jtr.

#include <iostream>

constexpr long long sq(long long x) { return x * x; }

constexpr long long fib(long long n) {
    return n < 2 ? n : fib(n - 1) + fib(n - 2);
}

constexpr long long squares[] = {sq(0), sq(1), sq(2), sq(3), sq(4), sq(5)};
constexpr long long fibs[] = {fib(1), fib(2), fib(3), fib(4), fib(5)};

int main() {
    std::cout << squares[5] << '\n';
    std::cout << (sizeof(squares) / sizeof(squares[0])) << '\n';
    std::cout << fibs[2] << '\n';
    std::cout << (1 + 2) << '\n';
}
