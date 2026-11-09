{ pkgs }:
let
  inherit (pkgs) lib;
  buildInputs =
    with pkgs;
    [
      xz
      bzip2
      zlib
    ]
    ++ lib.optionals stdenv.hostPlatform.isLinux [
      fontconfig
      freetype
      libxcb
      libxkbcommon
      wayland
      vulkan-loader
      libGL
    ];
in
{
  inherit buildInputs;
  nativeBuildInputs = with pkgs; [
    pkg-config
    clang
    rustPlatform.bindgenHook
  ];
  libraryPath = lib.makeLibraryPath buildInputs;
}
