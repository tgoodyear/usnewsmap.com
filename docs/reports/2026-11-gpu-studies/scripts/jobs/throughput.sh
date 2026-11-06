#!/usr/bin/env bash
# Study 4 in one image: reads the page batch's crops with Qwen when the Qwen image is in use, or its pages with Surya.
set -u
case "$(cat /work/in/kind.txt)" in
  qwen) bash /work/in/qwen.sh;;
  surya) bash /work/in/surya.sh;;
esac
