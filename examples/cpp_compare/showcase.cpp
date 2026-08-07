// Bundled C++17 showcase matching showcase.jtr.

#include <cassert>
#include <cerrno>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <iostream>
#include <memory>
#include <variant>
#include <vector>

struct Expr;
struct Lit { std::int32_t v; };
struct Add { std::unique_ptr<Expr> l; std::unique_ptr<Expr> r; };
struct Neg { std::unique_ptr<Expr> x; };
struct Expr { std::variant<Lit, Add, Neg> node; };

static std::int32_t eval(const Expr& e) {
    return std::visit([](const auto& node) -> std::int32_t {
        using T = std::decay_t<decltype(node)>;
        if constexpr (std::is_same_v<T, Lit>) {
            return node.v;
        } else if constexpr (std::is_same_v<T, Add>) {
            return eval(*node.l) + eval(*node.r);
        } else {
            return -eval(*node.x);
        }
    }, e.node);
}

static int clamp(int x, int lo, int hi) {
    assert(lo <= hi);
    int result = x < lo ? lo : (x > hi ? hi : x);
    assert(result >= lo);
    assert(result <= hi);
    return result;
}

struct Natural { std::uint8_t tag; std::int32_t value; std::uint8_t flags; };
#pragma pack(push, 1)
struct Packed { std::uint8_t tag; std::int32_t value; std::uint8_t flags; };
#pragma pack(pop)
struct alignas(16) CacheLineHint { std::int32_t value; };

struct Allocator {
    void* ctx;
    void* (*alloc)(void*, std::size_t);
    void (*free)(void*, void*);
};

static void* sys_alloc(void*, std::size_t bytes) { return std::malloc(bytes); }
static void sys_free(void*, void* p) { std::free(p); }
static Allocator system_allocator() { return Allocator{nullptr, sys_alloc, sys_free}; }

class IntList {
public:
    explicit IntList(Allocator a) : a_(a) {}
    ~IntList() { if (cap_ != 0) a_.free(a_.ctx, data_); }
    void push(int value) {
        if (len_ == cap_) {
            const std::size_t next_cap = cap_ == 0 ? 4 : cap_ * 2;
            auto* next = static_cast<int*>(a_.alloc(a_.ctx, next_cap * sizeof(int)));
            for (std::size_t i = 0; i < len_; ++i) next[i] = data_[i];
            if (cap_ != 0) a_.free(a_.ctx, data_);
            data_ = next;
            cap_ = next_cap;
        }
        data_[len_++] = value;
    }
    int get(std::size_t i) const { return data_[i]; }
    std::size_t len() const { return len_; }
private:
    int* data_ = nullptr;
    std::size_t len_ = 0;
    std::size_t cap_ = 0;
    Allocator a_;
};

static int fill_and_sum(Allocator a, int n) {
    IntList xs(a);
    for (int i = 0; i < n; ++i) xs.push(i + 1);
    int total = 0;
    for (std::size_t i = 0; i < xs.len(); ++i) total += xs.get(i);
    return total;
}

class Region {
public:
    explicit Region(std::size_t cap) : data_(cap) {}
    template <class T>
    T* make(T value) {
        const std::size_t mask = alignof(T) - 1;
        used_ = (used_ + mask) & ~mask;
        if (used_ + sizeof(T) > data_.size()) std::abort();
        void* ptr = data_.data() + used_;
        used_ += sizeof(T);
        return new (ptr) T(value);
    }
private:
    std::vector<unsigned char> data_;
    std::size_t used_ = 0;
};

static long long parse_or(const char* text, long long fallback) {
    errno = 0;
    char* end = nullptr;
    const long long value = std::strtoll(text, &end, 10);
    if (errno == ERANGE || end == text || *end != '\0') return fallback;
    return value;
}

constexpr long long sq(long long x) { return x * x; }
constexpr long long squares[] = {sq(0), sq(1), sq(2), sq(3), sq(4), sq(5)};

static double naive_sum(const double* xs, std::size_t n) {
    double total = 0.0;
    for (std::size_t i = 0; i < n; ++i) total += xs[i];
    return total;
}

static double neumaier_sum(const double* xs, std::size_t n) {
    double sum = 0.0;
    double c = 0.0;
    for (std::size_t i = 0; i < n; ++i) {
        const double t = sum + xs[i];
        if (std::abs(sum) >= std::abs(xs[i])) c += (sum - t) + xs[i];
        else c += (xs[i] - t) + sum;
        sum = t;
    }
    return sum + c;
}

int main() {
    auto a = std::make_unique<Expr>(Expr{Lit{40}});
    auto two = std::make_unique<Expr>(Expr{Lit{2}});
    auto b = std::make_unique<Expr>(Expr{Neg{std::move(two)}});
    Expr root{Add{std::move(a), std::move(b)}};
    std::cout << eval(root) << '\n';
    std::cout << sizeof(std::int32_t*) << '\n';
    std::cout << sizeof(std::int32_t*) << '\n';

    std::cout << clamp(12, 0, 10) << '\n';
    std::cout << clamp(-4, 0, 10) << '\n';
    std::cout << clamp(7, 0, 10) << '\n';

    std::cout << sizeof(Natural) << '\n';
    std::cout << sizeof(Packed) << '\n';
    std::cout << alignof(CacheLineHint) << '\n';
    std::cout << offsetof(Natural, value) << '\n';
    std::cout << offsetof(Packed, value) << '\n';

    std::cout << fill_and_sum(system_allocator(), 1000) << '\n';

    Region r(1024);
    int* x = r.make(10);
    int* y = r.make(20);
    int* z = r.make(*x + *y);
    std::cout << *z << '\n';

    std::cout << parse_or("9999999999999999999", 7) << '\n';
    char buf[24];
    std::snprintf(buf, sizeof(buf), "%lld", -4271LL);
    std::cout << buf << '\n';

    std::cout << squares[5] << '\n';

    double fs[] = {1.0, 1.0e100, 1.0, -1.0e100};
    std::cout << naive_sum(fs, 4) << '\n';
    std::cout << neumaier_sum(fs, 4) << '\n';
}
