// Byte layout probe in C++17: close equivalent to layout_probe.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -o "$env:TEMP\layout_probe_cpp.exe" examples/cpp_compare/layout_probe.cpp

#include <cstddef>
#include <cstdint>
#include <iostream>

struct Natural {
    std::uint8_t tag;
    std::int32_t value;
    std::uint8_t flags;
};

#pragma pack(push, 1)
struct Packed {
    std::uint8_t tag;
    std::int32_t value;
    std::uint8_t flags;
};
#pragma pack(pop)

struct alignas(16) CacheLineHint {
    std::int32_t value;
};

int main() {
    std::cout << sizeof(Natural) << '\n';
    std::cout << sizeof(Packed) << '\n';
    std::cout << alignof(CacheLineHint) << '\n';
    std::cout << offsetof(Natural, value) << '\n';
    std::cout << offsetof(Packed, value) << '\n';
}
