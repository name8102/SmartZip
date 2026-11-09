{
  lib,
  stdenv,
  fetchurl,
}:
let
  source = builtins.fromJSON (builtins.readFile ./sevenzip-source.json);
in
stdenv.mkDerivation {
  pname = "smartzip-sevenzip";
  inherit (source) version;
  src = fetchurl { inherit (source) url hash; };
  sourceRoot = ".";
  strictDeps = true;
  buildPhase = ''
    runHook preBuild
    make -C CPP/7zip/Bundles/Format7zF -f ../../cmpl_gcc.mak \
      -j"$NIX_BUILD_CORES" O=_o USE_ASM= CFLAGS_WARN_WALL="-Wall -Wextra" CC="$CC" CXX="$CXX"
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    install -Dm755 CPP/7zip/Bundles/Format7zF/_o/7z.so "$out/lib/smartzip/7z.so"
    for license in License.txt copying.txt unRarLicense.txt; do
      install -Dm644 "DOC/$license" "$out/share/licenses/7zip/$license"
    done
    install -Dm644 ${./sevenzip-source.json} "$out/share/licenses/7zip/source.json"
    install -Dm644 "$src" "$out/share/smartzip/7zip-source.tar.xz"
    runHook postInstall
  '';
  meta = {
    description = "Official full-format 7-Zip library with RAR decoding";
    homepage = "https://www.7-zip.org/";
    license = [
      lib.licenses.lgpl21Plus
      lib.licenses.unfreeRedistributable
    ];
    platforms = lib.platforms.linux;
  };
}
