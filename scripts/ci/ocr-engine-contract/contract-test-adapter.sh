#!/bin/sh
set -eu

printf '%s\n' '{"adapter":{"lang":"sh","name":"cgroup-acceptance","version":"1"},"cancel":"kill","contract":1,"deterministic":true,"engine":{"family":"tesseract","id":"tesseract-5.5","version":"5.5.0"},"granularity":"page","layout":"boxes","models":[{"name":"tessdata/eng.traineddata","sha256":"8280aed0782fe27257a68ea10fe7ef324ca0f8d85bd2fd145d1c2b560bcb66ba"}],"models_loaded":"at_start","profiles":[{"id":"eng","languages":["eng"],"models":["tessdata/eng.traineddata"],"tunable":[]}],"type":"hello"}'

while IFS= read -r request; do
    field() {
        printf '%s\n' "$request" | sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p"
    }
    number() {
        printf '%s\n' "$request" | sed -n "s/.*\"$1\":\\([0-9][0-9]*\\).*/\\1/p"
    }

    run_id=$(field run_id)
    out=$(field out)
    image=$(field path)
    profile=$(field profile)
    width=$(number width)
    height=$(number height)
    [ -n "$run_id" ] && [ -n "$out" ] && [ -n "$image" ] && [ -n "$profile" ]
    [ -n "$width" ] && [ -n "$height" ]

    if [ "$profile" != eng ]; then
        printf '{"engine_stats":{},"error":{"kind":"unsupported","message":"unknown profile"},"markup":"none","run_id":"%s","status":"unsupported","type":"result","warnings":[]}\n' "$run_id"
        continue
    fi

    if [ "$width" -gt 3000 ]; then
        sleep 60 &
        wait
        continue
    fi

    printf '%s\n' 'tesseract --dpi 300' >&2
    if ! text=$(tesseract "$image" stdout -l eng --dpi 300 2>/dev/null); then
        printf '{"engine_stats":{},"error":{"kind":"bad_input","message":"image rejected"},"markup":"none","run_id":"%s","status":"bad_input","type":"result","warnings":[]}\n' "$run_id"
        continue
    fi
    printf '%s\n' "$text" > "${out}page.txt"
    case "$text" in
        '') blocks='[]' ;;
        'HELLO LOWER') blocks='[{"bbox":[40,40,430,82],"kind":"text","text":"HELLO LOWER"}]' ;;
        *)
            printf '{"engine_stats":{},"error":{"kind":"failed","message":"unexpected OCR text"},"markup":"none","run_id":"%s","status":"failed","type":"result","warnings":[]}\n' "$run_id"
            continue
            ;;
    esac
    printf '{"v":1,"units":"px","frame":{"width":%s,"height":%s},"granularity":"page","reading_order":"engine","blocks":%s}' \
        "$width" "$height" "$blocks" > "${out}boxes.json"
    printf '{"engine_stats":{},"layout":{"kind":"boxes","path":"%sboxes.json"},"markup":"none","run_id":"%s","status":"ok","text":{"path":"%spage.txt"},"type":"result","warnings":[]}\n' \
        "$out" "$run_id" "$out"
done
