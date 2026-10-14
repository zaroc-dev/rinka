Icons rendered from `assets/rinka.svg` (the Nix package only sees `app/`):

```sh
for n in 16 24 32 48 64 128 256; do
  nix run nixpkgs#librsvg -- -w $n -h $n assets/rinka.svg -o /tmp/rinka-$n.png
done
cp /tmp/rinka-256.png app/resources/icon.png
nix shell nixpkgs#imagemagick -c magick /tmp/rinka-{16,24,32,48,64,128,256}.png app/resources/icon.ico
```
