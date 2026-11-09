#ifndef USNM_GOLD_ENGINE_ROUTING_H
#define USNM_GOLD_ENGINE_ROUTING_H

#include <fstream>
#include <functional>
#include <regex>
#include <stdexcept>
#include <string>
#include <nlohmann/json.hpp>

namespace usnm_gold {

inline std::string read_file(const std::string& path) {
    std::ifstream file(path, std::ios::binary | std::ios::ate);
    if (!file) throw std::runtime_error("cannot open Gold routing input");
    const auto size = file.tellg();
    if (size < 0 || size > 1024 * 1024) {
        throw std::runtime_error("Gold routing input exceeds size limit");
    }
    std::string text(static_cast<std::size_t>(size), '\0');
    file.seekg(0);
    if (!text.empty() && !file.read(&text[0], text.size())) {
        throw std::runtime_error("cannot read Gold routing input");
    }
    return text;
}

inline std::string tag(const std::string& xml, const std::string& name) {
    const std::regex pattern("<" + name + ">\\s*([^<]*?)\\s*</" + name + ">");
    std::sregex_iterator it(xml.begin(), xml.end(), pattern), end;
    if (it == end) throw std::runtime_error("Gold app file reference lacks " + name);
    const std::string value = (*it)[1].str();
    if (++it != end) throw std::runtime_error("Gold app file reference repeats " + name);
    return value;
}

inline bool compatible(
    const std::string& workunit,
    const std::string& platform,
    const std::string& version_xml,
    const nlohmann::json& bindings,
    const std::function<std::string(const std::string&)>& download_path,
    const std::function<std::string(const std::string&)>& application_path,
    bool native = false
) {
    static const std::regex name("^usnm_gold_([0-9a-f]{64})$");
    std::smatch match;
    if (!std::regex_match(workunit, match, name)) {
        throw std::runtime_error("invalid Gold workunit identity");
    }
    const auto job = nlohmann::json::parse(read_file(download_path(match[1].str() + "_gold_json")));
    if (job.at("id") != match[1].str()) {
        throw std::runtime_error("Gold staged job identity differs from workunit");
    }
    const std::string image = job.at("engine").at("image");
    const std::string base = job.at("engine").at("base");
    const auto engine = bindings.find(image);
    if (engine == bindings.end() || !engine->is_object()
        || !engine->contains(platform) || engine->at(platform) != base) {
        throw std::runtime_error("Gold image/base has no compatible reviewed platform binding");
    }

    const std::regex refs("<file_ref>([\\s\\S]*?)</file_ref>");
    const std::string open_name = native ? "usnm_native_build.json" : "Dockerfile";
    std::string dockerfile;
    for (std::sregex_iterator it(version_xml.begin(), version_xml.end(), refs), end; it != end; ++it) {
        const std::string ref = (*it)[1].str();
        if (ref.find("<open_name>") == std::string::npos || tag(ref, "open_name") != open_name) continue;
        if (!dockerfile.empty()) throw std::runtime_error("multiple Gold " + open_name + " references");
        dockerfile = tag(ref, "file_name");
    }
    if (native) {
        // A native build's identity is the digest of its manifest, which the file's physical
        // name carries; the worker hashes the manifest again and refuses any other job.
        if (dockerfile != "usnm_native_build_" + image.substr(image.find(':') + 1) + ".json") {
            throw std::runtime_error("Gold job image differs from registered native build");
        }
        return true;
    }
    if (dockerfile.empty() || dockerfile.find_first_not_of(
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-") != std::string::npos) {
        throw std::runtime_error("missing or invalid Gold Dockerfile reference");
    }
    const std::string payload = read_file(application_path(dockerfile));
    const std::string expected = "FROM ghcr.io/tgoodyear/usnm-boinc-worker@" + image;
    if (payload != expected && payload != expected + "\n") {
        throw std::runtime_error("Gold job image differs from registered app Dockerfile");
    }
    return true;
}

} // namespace usnm_gold
#endif
