// Native boundary for the pinned b8799 ggml ABI and the OpenCL ICD API.
// No GPU inference or ordinal fallback: emit one PCI-verified Vulkan device or fail.
#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstring>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <stdexcept>
#include <string>
#include <vector>
#ifdef _WIN32
#include <windows.h>
#define CL_CALL __stdcall
#else
#include <dlfcn.h>
#define CL_CALL
#endif

struct Library {
#ifdef _WIN32
    HMODULE handle;
    explicit Library(const std::string &path): handle(LoadLibraryA(path.c_str())) {
#else
    void *handle;
    explicit Library(const std::string &path): handle(dlopen(path.c_str(), RTLD_NOW | RTLD_GLOBAL)) {
#endif
        if (!handle) {
#ifdef _WIN32
            throw std::runtime_error("identity library unavailable: " + path + " (Windows error " + std::to_string(GetLastError()) + ")");
#else
            throw std::runtime_error("identity library unavailable: " + path + ": " + dlerror());
#endif
        }
    }
    template<class T> T symbol(const char *name) {
#ifdef _WIN32
        auto value = GetProcAddress(handle, name);
#else
        auto value = dlsym(handle, name);
#endif
        if (!value) throw std::runtime_error(std::string("identity API unavailable: ") + name);
        static_assert(sizeof(T) == sizeof(value));
        T function;
        std::memcpy(&function, &value, sizeof(function));
        return function;
    }
};

struct Pci {
    uint32_t domain, bus, device, function;
    bool domain_known;
};
static bool matches(const Pci &a, const Pci &b) {
    return (!a.domain_known || !b.domain_known || a.domain == b.domain)
        && a.bus == b.bus && a.device == b.device && a.function == b.function;
}
static Pci parse_pci(const char *text) {
    Pci value{};
    int end = 0;
    if (!text || std::sscanf(text, "%x:%x:%x.%x%n", &value.domain, &value.bus,
                            &value.device, &value.function, &end) != 4 || text[end]
            || value.bus > 255 || value.device > 31 || value.function > 7) {
        throw std::runtime_error("missing or invalid Vulkan PCI identity");
    }
    value.domain_known = true;
    return value;
}

// Layout of ggml_backend_dev_props/caps in the unchanged b8799 ggml-backend.h.
struct Props {
    const char *name, *description;
    size_t memory_free, memory_total;
    int type;
    const char *device_id;
    struct { bool async, host_buffer, buffer_from_host_ptr, events; } caps;
};
struct Adapter { std::string name; Pci pci; };

static std::string unique_match(const std::vector<Pci> &assigned, const std::vector<Adapter> &devices) {
    if (assigned.empty()) throw std::runtime_error("assigned OpenCL device has no PCI identity");
    std::string selected;
    for (const auto &identity : assigned) {
        std::vector<std::string> found;
        for (const auto &device : devices) if (matches(identity, device.pci)) found.push_back(device.name);
        if (found.size() != 1 || (!selected.empty() && selected != found[0])) {
            throw std::runtime_error("missing or ambiguous assigned Vulkan device");
        }
        selected = found[0];
    }
    return selected;
}

static std::vector<Pci> opencl_assignment(uint32_t index) {
#ifdef _WIN32
    Library cl("OpenCL.dll");
#else
    Library cl("libOpenCL.so.1");
#endif
    auto platforms = cl.symbol<int(CL_CALL *)(uint32_t, void **, uint32_t *)>("clGetPlatformIDs");
    auto devices = cl.symbol<int(CL_CALL *)(void *, uint64_t, uint32_t, void **, uint32_t *)>("clGetDeviceIDs");
    auto info = cl.symbol<int(CL_CALL *)(void *, uint32_t, size_t, void *, size_t *)>("clGetDeviceInfo");
    void *ids[64]{};
    uint32_t count = 0;
    if (platforms(64, ids, &count) || count > 64) throw std::runtime_error("OpenCL platforms unavailable");
    std::vector<Pci> assigned;
    for (uint32_t p = 0; p < count; ++p) {
        void *gpus[256]{};
        uint32_t n = 0;
        if (devices(ids[p], 4, 256, gpus, &n) || n > 256 || index >= n) continue;
        uint32_t vendor = 0;
        if (info(gpus[index], 0x1001, sizeof(vendor), &vendor, nullptr) || vendor != 0x1002) continue;
        // gpu_opencl_dev_index indexes all GPU devices on each platform, as in BOINC's
        // boinc_get_opencl_ids_aux. gpu_device_num is not an OpenCL/Vulkan index.
        Pci pci{};
        uint32_t khr[4]{};
        if (!info(gpus[index], 0x410F, sizeof(khr), khr, nullptr)) {
            pci = {khr[0], khr[1], khr[2], khr[3], true};
        } else {
            // CL_DEVICE_TOPOLOGY_AMD has no PCI domain. Matching must remain unique
            // across every Vulkan domain; otherwise refuse rather than guess domain 0.
            struct { uint32_t type; unsigned char data[20]; } topology{};
            if (info(gpus[index], 0x4037, sizeof(topology), &topology, nullptr) || topology.type != 1) {
                throw std::runtime_error("assigned AMD OpenCL device has no PCI topology");
            }
            pci = {0, topology.data[17], topology.data[18], topology.data[19], false};
        }
        if (pci.bus > 255 || pci.device > 31 || pci.function > 7) throw std::runtime_error("invalid OpenCL PCI topology");
        assigned.push_back(pci);
    }
    return assigned;
}

static std::vector<Adapter> vulkan_devices(const std::filesystem::path &dir) {
#ifdef _WIN32
    Library base((dir / "ggml-base.dll").string());
    Library registry((dir / "ggml.dll").string());
    auto backend = (dir / "ggml-vulkan.dll").string();
#else
    Library base((dir / "libggml-base.so").string());
    Library registry((dir / "libggml.so").string());
    auto backend = (dir / "libggml-vulkan.so").string();
#endif
    auto load = registry.symbol<void *(*)(const char *)>("ggml_backend_load");
    auto count = base.symbol<size_t(*)(void *)>("ggml_backend_reg_dev_count");
    auto device = base.symbol<void *(*)(void *, size_t)>("ggml_backend_reg_dev_get");
    auto props = base.symbol<void(*)(void *, Props *)>("ggml_backend_dev_get_props");
    void *reg = load(backend.c_str());
    if (!reg) throw std::runtime_error("pinned Vulkan backend unavailable");
    size_t n = count(reg);
    if (n > 256) throw std::runtime_error("too many Vulkan devices");
    std::vector<Adapter> result;
    for (size_t i = 0; i < n; ++i) {
        Props value{};
        props(device(reg, i), &value);
        if (!value.name || !value.description || !value.device_id) continue;
        std::string name(value.name), description(value.description);
        std::transform(description.begin(), description.end(), description.begin(),
                       [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
        if (description.find("llvmpipe") != std::string::npos || description.find("lavapipe") != std::string::npos
                || description.find("swiftshader") != std::string::npos) continue;
        if (name.rfind("Vulkan", 0) != 0) continue;
        result.push_back({name, parse_pci(value.device_id)});
    }
    return result;
}

int main(int argc, char **argv) {
    try {
        if (argc == 2 && std::string(argv[1]) == "--self-test") {
            auto a = parse_pci("0000:03:00.0"), b = parse_pci("0000:04:00.0");
            if (unique_match({a}, {{"Vulkan5", b}, {"Vulkan2", a}}) != "Vulkan2") return 1;
            for (const auto &devices : std::vector<std::vector<Adapter>>{
                    {{"Vulkan0", b}}, {{"Vulkan0", a}, {"Vulkan1", a}}, {}}) {
                bool refused = false;
                try { unique_match({a}, devices); } catch (const std::exception &) { refused = true; }
                if (!refused) return 1;
            }
            for (const auto &identities : std::vector<std::vector<Pci>>{{}, {a, b}}) {
                bool refused = false;
                try { unique_match(identities, {{"Vulkan0", a}, {"Vulkan1", b}}); }
                catch (const std::exception &) { refused = true; }
                if (!refused) return 1;
            }
            bool refused = false;
            a.domain_known = false;
            auto other_domain = parse_pci("0001:03:00.0");
            try { unique_match({a}, {{"Vulkan0", a}, {"Vulkan1", other_domain}}); }
            catch (const std::exception &) { refused = true; }
            return refused ? 0 : 1;
        }
        if (argc != 3) throw std::runtime_error("expected bundle directory and OpenCL device index");
        std::string raw(argv[2]);
        if (raw.empty() || raw.find_first_not_of("0123456789") != std::string::npos) throw std::runtime_error("invalid OpenCL index");
        auto index = std::stoul(raw);
        if (index > 255) throw std::runtime_error("invalid OpenCL index");
        auto assigned = opencl_assignment(static_cast<uint32_t>(index));
        auto selected = unique_match(assigned, vulkan_devices(std::filesystem::absolute(argv[1])));
        std::printf("%s\n", selected.c_str());
        return 0;
    } catch (const std::exception &error) {
        std::fprintf(stderr, "AMD identity refusal: %s\n", error.what());
        return 1;
    }
}
