// Allocator-backed collection in C++17: close equivalent to allocator_list.jtr.
//
// Build:
//   g++ -O2 -std=c++17 -o "$env:TEMP\allocator_list_cpp.exe" examples/cpp_compare/allocator_list.cpp

#include <cstddef>
#include <cstdlib>
#include <iostream>

struct Allocator {
    void* ctx;
    void* (*alloc)(void* ctx, std::size_t bytes);
    void (*free)(void* ctx, void* ptr);
    void (*destroy)(void* ctx);
};

static void* sys_alloc(void*, std::size_t bytes) { return std::malloc(bytes); }
static void sys_free(void*, void* ptr) { std::free(ptr); }
static void sys_destroy(void*) {}

static Allocator system_allocator() {
    return Allocator{nullptr, sys_alloc, sys_free, sys_destroy};
}

struct Arena {
    char* data;
    std::size_t cap;
    std::size_t used;
};

static void* arena_alloc(void* ctx, std::size_t bytes) {
    auto* arena = static_cast<Arena*>(ctx);
    if (arena->used + bytes > arena->cap) {
        std::abort();
    }
    void* ptr = arena->data + arena->used;
    arena->used += bytes;
    return ptr;
}

static void arena_free(void*, void*) {}

static void arena_destroy(void* ctx) {
    auto* arena = static_cast<Arena*>(ctx);
    std::free(arena->data);
    delete arena;
}

static Allocator arena_allocator(std::size_t cap) {
    auto* arena = new Arena{static_cast<char*>(std::malloc(cap)), cap, 0};
    return Allocator{arena, arena_alloc, arena_free, arena_destroy};
}

class IntList {
public:
    explicit IntList(Allocator allocator) : allocator_(allocator) {}
    ~IntList() {
        if (cap_ != 0) {
            allocator_.free(allocator_.ctx, data_);
        }
    }

    void push(int value) {
        if (len_ == cap_) {
            const std::size_t new_cap = cap_ == 0 ? 4 : cap_ * 2;
            auto* next = static_cast<int*>(allocator_.alloc(allocator_.ctx, new_cap * sizeof(int)));
            for (std::size_t i = 0; i < len_; ++i) {
                next[i] = data_[i];
            }
            if (cap_ != 0) {
                allocator_.free(allocator_.ctx, data_);
            }
            data_ = next;
            cap_ = new_cap;
        }
        data_[len_++] = value;
    }

    int get(std::size_t i) const { return data_[i]; }
    std::size_t len() const { return len_; }

private:
    int* data_ = nullptr;
    std::size_t len_ = 0;
    std::size_t cap_ = 0;
    Allocator allocator_;
};

static int fill_and_sum(Allocator allocator, int n) {
    IntList xs(allocator);
    for (int i = 0; i < n; ++i) {
        xs.push(i + 1);
    }

    int total = 0;
    for (std::size_t i = 0; i < xs.len(); ++i) {
        total += xs.get(i);
    }
    return total;
}

int main() {
    Allocator sys = system_allocator();
    std::cout << fill_and_sum(sys, 1000) << '\n';

    Allocator arena = arena_allocator(65536);
    std::cout << fill_and_sum(arena, 1000) << '\n';
    arena.destroy(arena.ctx);
}
