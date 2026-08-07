// Lexical arena allocation in C++17: close equivalent to region_arena.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -o "$env:TEMP\region_arena_cpp.exe" examples\cpp_compare\region_arena.cpp

#include <cstddef>
#include <cstdlib>
#include <iostream>
#include <new>
#include <vector>

class Region {
public:
    explicit Region(std::size_t cap) : data_(cap) {}

    template <class T>
    T* make(T value) {
        const std::size_t align = alignof(T);
        const std::size_t mask = align - 1;
        used_ = (used_ + mask) & ~mask;
        if (used_ + sizeof(T) > data_.size()) {
            std::abort();
        }
        void* ptr = data_.data() + used_;
        used_ += sizeof(T);
        return new (ptr) T(value);
    }

private:
    std::vector<unsigned char> data_;
    std::size_t used_ = 0;
};

int main() {
    Region r(1024);
    int* a = r.make(10);
    int* b = r.make(20);
    int* c = r.make(*a + *b);

    std::cout << *a << '\n';
    std::cout << *b << '\n';
    std::cout << *c << '\n';
}
