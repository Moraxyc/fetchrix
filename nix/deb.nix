{
  lib,
  stdenvNoCC,
  replaceVars,
  dpkg,
  fetchrix-static,
}:
let
  inherit (fetchrix-static) pname version;
  debVersion = "${version}-1";
  architecture =
    {
      x86_64-linux = "amd64";
      aarch64-linux = "arm64";
    }
    .${stdenvNoCC.hostPlatform.system};
  control = replaceVars ./debian/control {
    inherit pname architecture;
    inherit (fetchrix-static.meta) homepage description;
    version = debVersion;
    installedSize = null;
  };
in
stdenvNoCC.mkDerivation {
  pname = "${pname}-deb";
  inherit version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../LICENSE-APACHE
      ../LICENSE-MIT
    ];
  };

  nativeBuildInputs = [ dpkg ];

  buildPhase = ''
    runHook preBuild
    install -Dm755 ${lib.getExe fetchrix-static} root/usr/bin/${pname}
    install -Dm644 -t root/usr/share/doc/${pname} LICENSE-APACHE LICENSE-MIT
    install -Dm644 ${control} root/DEBIAN/control
    substituteInPlace root/DEBIAN/control \
      --replace-fail @installedSize@ "$(du -sk --exclude=DEBIAN root | cut -f1)"
    dpkg-deb --root-owner-group -Zxz --build root ${pname}_${debVersion}_${architecture}.deb
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -Dm644 -t $out *.deb
    runHook postInstall
  '';
}
