// This header is compiled only into the feeder. It reports, but never changes, its slot map.
#ifndef USNM_FEEDER_GUARD_H
#define USNM_FEEDER_GUARD_H

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <limits>
#include <sstream>
#include <string>
#include <vector>
#include <fcntl.h>
#include <time.h>
#include <unistd.h>

class UsnmFeederGuard {
    struct App {
        long id;
        double weight;
    };

    std::vector<App> apps;
    std::string path, start_ticks, boot_id;
    unsigned long pid = 0;
    unsigned long long sequence = 0, next_heartbeat = 0;
    bool enabled = false, active = false;
    bool drift = false, uncertain = false, reloading = false;

    static bool digits(const std::string& s) {
        if (s.empty()) return false;
        for (char c : s) if (c < '0' || c > '9') return false;
        return true;
    }

    static bool boot_id_valid(const std::string& s) {
        if (s.size() != 36) return false;
        for (size_t i = 0; i < s.size(); i++) {
            if (i == 8 || i == 13 || i == 18 || i == 23) {
                if (s[i] != '-') return false;
            } else if (!((s[i] >= '0' && s[i] <= '9') ||
                         (s[i] >= 'a' && s[i] <= 'f'))) {
                return false;
            }
        }
        return true;
    }

    static unsigned long long monotonic_ms() {
        struct timespec ts;
        if (clock_gettime(CLOCK_MONOTONIC, &ts) || ts.tv_sec < 0) return 0;
        return static_cast<unsigned long long>(ts.tv_sec) * 1000 + ts.tv_nsec / 1000000;
    }

    void failure() noexcept {
        uncertain = true;
        if (!path.empty()) unlink(path.c_str());
        std::fprintf(stderr, "usnm-feeder-guard: allocation status unavailable\n");
    }

    static bool valid_app(const APP& app) {
        return app.id > 0 && std::isfinite(app.weight) && app.weight >= 0;
    }

    void publish(unsigned long long now) {
        if (!active) return;
        if (!now) {
            failure();
            return;
        }
        next_heartbeat = now + 60000;
        sequence++;
        const std::string stage = path + "." + std::to_string(pid) + "." +
            start_ticks + "." + std::to_string(sequence) + ".new";
        int fd = open(stage.c_str(), O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC | O_NOFOLLOW, 0640);
        if (fd < 0) {
            failure();
            return;
        }
        FILE* file = fdopen(fd, "w");
        if (!file) {
            close(fd);
            unlink(stage.c_str());
            failure();
            return;
        }
        bool ok = std::fprintf(file,
            "{\"version\":1,\"pid\":%lu,\"start_ticks\":\"%s\",\"boot_id\":\"%s\","
            "\"sequence\":%llu,\"monotonic_ms\":%llu,"
            "\"drift\":%s,\"uncertain\":%s,\"reloading\":%s,\"apps\":[",
            pid, start_ticks.c_str(), boot_id.c_str(), sequence, now,
            drift ? "true" : "false", uncertain ? "true" : "false",
            reloading ? "true" : "false") >= 0;
        for (size_t i = 0; i < apps.size(); i++) {
            // Encode the actual binary double, not show_shmem's rounded %.2f weight.
            double weight = apps[i].weight == 0 ? 0 : apps[i].weight;
            uint64_t bits = 0;
            std::memcpy(&bits, &weight, sizeof(bits));
            ok = std::fprintf(file, "%s[%ld,\"%016llx\"]", i ? "," : "",
                apps[i].id, static_cast<unsigned long long>(bits)) >= 0 && ok;
        }
        ok = std::fprintf(file, "]}\n") >= 0 && ok;
        if (std::fflush(file)) ok = false;
        if (std::fclose(file)) ok = false;
        if (ok && !std::rename(stage.c_str(), path.c_str())) return;
        unlink(stage.c_str());
        failure();
    }

    void capture_impl(const SCHED_SHMEM* ssp, bool all_apps, int scan_result) {
        enabled = all_apps;
        if (!enabled) return;
        if (scan_result || ssp->napps <= 0 || ssp->napps > MAX_APPS ||
            sizeof(double) != sizeof(uint64_t) || !std::numeric_limits<double>::is_iec559) {
            uncertain = true;
            return;
        }
        for (int i = 0; i < ssp->napps; i++) {
            const APP& app = ssp->apps[i];
            if (!valid_app(app)) uncertain = true;
            for (const App& before : apps) if (before.id == app.id) uncertain = true;
            apps.push_back({app.id, app.weight});
        }
    }

public:
    void capture(const SCHED_SHMEM* ssp, bool all_apps, int scan_result) noexcept {
        try {
            capture_impl(ssp, all_apps, scan_result);
        } catch (...) {
            enabled = false;
            failure();
        }
    }

    // Kept separate from start() so the real state transitions can also be exercised off Linux.
    void activate(const std::string& filename, unsigned long process, const std::string& ticks,
                  const std::string& boot, unsigned long long now) noexcept {
        if (!enabled) return;
        try {
            path = filename;
            pid = process;
            start_ticks = ticks;
            boot_id = boot;
            if (!pid || !digits(start_ticks) || !boot_id_valid(boot_id)) {
                failure();
                return;
            }
            active = true;
            publish(now);
        } catch (...) {
            failure();
        }
    }

    void start() noexcept {
        if (!enabled) return;
        try {
            char hostname[256] = {};
            if (gethostname(hostname, sizeof(hostname) - 1)) {
                failure();
                return;
            }
            char* dot = std::strchr(hostname, '.');
            if (dot) *dot = 0;
            if (!hostname[0] || std::strchr(hostname, '/')) {
                failure();
                return;
            }
            std::ifstream stat("/proc/self/stat");
            std::string line, ticks, boot;
            std::getline(stat, line);
            size_t end = line.rfind(')');
            if (!stat || end == std::string::npos || end + 2 >= line.size()) {
                failure();
                return;
            }
            std::istringstream fields(line.substr(end + 2));
            for (int field = 3; field <= 22; field++) {
                if (!(fields >> ticks)) {
                    failure();
                    return;
                }
            }
            std::ifstream boot_file("/proc/sys/kernel/random/boot_id");
            if (!(boot_file >> boot)) {
                failure();
                return;
            }
            const char* root = std::getenv("USNM_PID_ROOT");
            if (!root || !root[0]) root = "/var/lib/usnm-boinc";
            if (root[0] != '/') {
                failure();
                return;
            }
            activate(std::string(root) + "/pid_" + hostname + "/usnm-feeder-slots.json",
                     static_cast<unsigned long>(getpid()), ticks, boot, monotonic_ms());
        } catch (...) {
            failure();
        }
    }

    void begin_reload() noexcept {
        if (!active) return;
        try {
            reloading = true;
            publish(monotonic_ms());
        } catch (...) {
            failure();
        }
    }

    void end_reload(const SCHED_SHMEM* ssp, int scan_result) noexcept {
        if (!active) return;
        try {
            if (scan_result || ssp->napps < 0 || ssp->napps > MAX_APPS) {
                uncertain = true;
            } else if (static_cast<size_t>(ssp->napps) != apps.size()) {
                drift = true;
            } else {
                for (int i = 0; i < ssp->napps; i++) {
                    const APP& app = ssp->apps[i];
                    if (!valid_app(app)) uncertain = true;
                    if (app.id != apps[i].id || app.weight != apps[i].weight) drift = true;
                }
            }
            reloading = false;
            publish(monotonic_ms());
        } catch (...) {
            failure();
        }
    }

    void heartbeat() noexcept {
        if (!active) return;
        try {
            unsigned long long now = monotonic_ms();
            if (!now || now >= next_heartbeat) publish(now);
        } catch (...) {
            failure();
        }
    }
};

#endif
