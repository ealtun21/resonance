#!/bin/sh
# usage: macshot.sh [sleep] ; writes /tmp/mac.png (960x540)
sleep ${1:-3}
echo "screendump /tmp/mac.ppm" | socat - unix-connect:/home/nyverino/.cache/resonance-e2e/spike-macos/macos-sequoia/macos-sequoia-monitor.socket >/dev/null 2>&1
sleep 1
python3 -c "
from PIL import Image
Image.open('/tmp/mac.ppm').resize((960,540)).save('/tmp/mac.png')"
