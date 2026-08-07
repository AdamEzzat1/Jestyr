// Deterministic integer parse/format in C++17: close equivalent to deterministic_numbers.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -o "$env:TEMP\deterministic_numbers_cpp.exe" examples\cpp_compare\deterministic_numbers.cpp

#include <cerrno>
#include <climits>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>

static long long parse_or(const char* text, long long fallback) {
    errno = 0;
    char* end = nullptr;
    const long long value = std::strtoll(text, &end, 10);
    if (errno == ERANGE || end == text || *end != '\0') {
        return fallback;
    }
    return value;
}

int main() {
    std::cout << parse_or("12345", 0) << '\n';
    std::cout << parse_or("-42", 0) << '\n';
    std::cout << parse_or("9999999999999999999", 7) << '\n';
    std::cout << parse_or("12x", 9) << '\n';

    char buf[24];
    std::snprintf(buf, sizeof(buf), "%lld", -4271LL);
    std::cout << buf << '\n';
    std::cout << parse_or(buf, 0) << '\n';
}
