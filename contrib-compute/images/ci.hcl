variable "CI_CACHE_WRITE" {
  default = "false"
}

group "ci" {
  targets = ["base", "server", "bridge-bin", "bridge"]
}

target "base" {
  context    = "."
  dockerfile = "contrib-compute/images/base/Containerfile"
  tags       = ["usnm-boinc-base:check"]
  output     = ["type=docker"]
  cache-from = ["type=gha,scope=boinc-base"]
  cache-to   = CI_CACHE_WRITE == "true" ? ["type=gha,scope=boinc-base,mode=max"] : []
}

target "server" {
  context    = "."
  dockerfile = "contrib-compute/images/server/Containerfile"
  tags       = ["usnm-boinc-server:check"]
  args       = { BASE_IMAGE = "usnm-boinc-base:check" }
  contexts   = { "usnm-boinc-base:check" = "target:base" }
  output     = ["type=docker"]
  cache-from = ["type=gha,scope=boinc-server"]
  cache-to   = CI_CACHE_WRITE == "true" ? ["type=gha,scope=boinc-server,mode=max"] : []
}

target "bridge-bin" {
  context    = "."
  dockerfile = "contrib-compute/images/bridge-bin/Containerfile"
  tags       = ["usnm-boinc-bridge-bin:check"]
  output     = ["type=docker"]
  cache-from = ["type=gha,scope=boinc-bridge-bin"]
  cache-to   = CI_CACHE_WRITE == "true" ? ["type=gha,scope=boinc-bridge-bin,mode=max"] : []
}

target "bridge" {
  context    = "."
  dockerfile = "contrib-compute/images/bridge/Containerfile"
  tags       = ["usnm-boinc-bridge:check"]
  args = {
    SERVER_IMAGE = "usnm-boinc-server:check"
    BIN_IMAGE    = "usnm-boinc-bridge-bin:check"
  }
  contexts = {
    "usnm-boinc-server:check"     = "target:server"
    "usnm-boinc-bridge-bin:check" = "target:bridge-bin"
  }
  output     = ["type=docker"]
  cache-from = ["type=gha,scope=boinc-bridge"]
  cache-to   = CI_CACHE_WRITE == "true" ? ["type=gha,scope=boinc-bridge,mode=max"] : []
}
