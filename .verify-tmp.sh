set -eux
cd /root/work
B=usnewsmap-usnewsmap-1683-apt-bases
M=usnm1683-mainctx
cd $B
rec=$(bash scripts/ci/apt-base.sh --app native --print-recipe)
# native: main, branch default, branch pinned (no network, mirrors blackholed)
podman build -q --network=host -t t/native-main -f ../$M/boinc/apps/native/Containerfile ../$M/boinc/apps/native
podman build -q --network=host --target apt-native --label org.usnewsmap.apt-recipe=$rec -t t/apt-native -f boinc/apps/native/Containerfile boinc/apps/native
podman build -q --network=host -t t/native-default -f boinc/apps/native/Containerfile boinc/apps/native
podman build -q --network=none --build-arg NATIVE_BASE=localhost/t/apt-native -t t/native-pinned -f boinc/apps/native/Containerfile boinc/apps/native
for i in main default pinned; do podman run --rm --network=none t/native-$i cat /out/usnm_tesseract.packages | sha256sum; podman run --rm t/native-$i sha256sum /out/usnm_tesseract; done
podman image inspect --format '{{index .Labels "org.usnewsmap.debian-snapshot"}} {{index .Labels "org.usnewsmap.apt-recipe"}}' t/apt-native
! podman build -q --network=none --build-arg SNAPSHOT=20260901T000000Z --build-arg NATIVE_BASE=localhost/t/apt-native -t t/native-bad -f boinc/apps/native/Containerfile boinc/apps/native
# iris
irec=$(bash scripts/ci/apt-base.sh --app iris --print-recipe)
podman build -q --network=host --target apt-iris-tools --label org.usnewsmap.apt-recipe=$irec -t t/iris-tools -f boinc/apps/iris/Containerfile .
podman build -q --network=host --target apt-iris --label org.usnewsmap.apt-recipe=$irec -t t/iris-apt -f boinc/apps/iris/Containerfile .
podman build -q --network=host -t t/iris-default -f boinc/apps/iris/Containerfile .
H="--add-host snapshot.debian.org:192.0.2.1 --add-host deb.debian.org:192.0.2.1 --add-host security.debian.org:192.0.2.1"
podman build -q --network=host $H --build-arg IRIS_TOOLS_BASE=localhost/t/iris-tools --build-arg IRIS_BASE=localhost/t/iris-apt -t t/iris-pinned -f boinc/apps/iris/Containerfile .
for i in default pinned; do podman run --rm --entrypoint cat t/iris-$i /opt/iris/dpkg.txt | sha256sum; done
podman build -q --network=host -t t/iris-main -f ../$M/boinc/apps/iris/Containerfile ../$M || true
podman run --rm --entrypoint cat t/iris-main /opt/iris/dpkg.txt | sha256sum
