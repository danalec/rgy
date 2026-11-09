{
  description = "rgy — whole-filesystem name search and content search in one stack";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      version = "0.1.10.2";
      base = "https://github.com/danalec/rgy/releases/download/rgy-v${version}";

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      # One release asset per platform. The digests are the SHA-256 of the
      # archive itself, in SRI form. They are rewritten from the combined
      # SHA256SUMS on publish day, together with the installer pins:
      # until the 0.1.10.2 archives exist, the values below are still the
      # 0.1.10.1 ones and `nix flake check` will (correctly) refuse them.
      # `nix flake check` verifies them against the real downloads once
      # the release is published.
      assets = {
        x86_64-linux = {
          file = "rgy-v${version}-x86_64-linux.tar.gz";
          hash = "sha256-z+H247rhpjlEblVm2UoFfwNpOi8LeLgY23p+/HinlSw=";
        };
        aarch64-linux = {
          file = "rgy-v${version}-aarch64-linux.tar.gz";
          hash = "sha256-zhI4nByLcPQzSNwUVCWOaT7r2vf4ojxW79GRgIWvfjM=";
        };
        x86_64-darwin = {
          file = "rgy-v${version}-x86_64-apple-darwin.tar.gz";
          hash = "sha256-ZGIiS036+qCYX6Gk3WOEJAJttiEkXC2Vz/c+a9EJWFk=";
        };
        aarch64-darwin = {
          file = "rgy-v${version}-aarch64-apple-darwin.tar.gz";
          hash = "sha256-jesHF0yTwfx0YsZohISdVrrP2SxTzo9hxRtN/+azu+8=";
        };
      };

      # Instruction-set tuned builds for x86_64 Linux. They are not the
      # default: the portable build selects its kernels at runtime.
      tuned = {
        x86_64-linux = {
          rgy-search-avx2 = {
            file = "rgy-v${version}-avx2-linux.tar.gz";
            hash = "sha256-rKLOsqjIjjjU1ulBCO06dHeuk8sr+OiW0R+Lhx75080=";
          };
          rgy-search-znver4 = {
            file = "rgy-v${version}-znver4-linux.tar.gz";
            hash = "sha256-e9myErnr8Z+EmIG3wENvwNtvQmHRndFepzPBlbdglG0=";
          };
        };
      };

      # The archive is flat: binaries, documents and the alias are at its
      # root, with no top-level directory. `rgy` is a symlink to searchctl,
      # so installing the link keeps one binary on disk and one name.
      mkRgy =
        pkgs: file: hash:
        pkgs.stdenv.mkDerivation {
          pname = "rgy-search";
          inherit version;

          src = pkgs.fetchurl {
            url = "${base}/${file}";
            inherit hash;
          };

          # The archive holds prebuilt ELF/Mach-O binaries, so on Linux the
          # interpreter and shared-library paths are rewritten for the store.
          nativeBuildInputs = pkgs.lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.autoPatchelfHook;
          buildInputs = pkgs.lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.stdenv.cc.cc.lib;

          sourceRoot = ".";
          dontBuild = true;
          dontFixup = pkgs.stdenv.hostPlatform.isDarwin;

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/bin" "$out/share/doc/rgy" "$out/share/licenses/rgy"
            for f in searchctl searchd rgy-autostart rgxy; do
              install -m755 "$f" "$out/bin/$f"
            done
            ln -s searchctl "$out/bin/rgy"
            for f in README.md MANUAL.md FORMATS.md COPYING NOTICE LICENSE-APACHE LICENSE-MIT; do
              if [ -f "$f" ]; then
                install -m644 "$f" "$out/share/doc/rgy/$f"
              fi
            done
            for f in COPYING NOTICE LICENSE-APACHE LICENSE-MIT; do
              if [ -f "$f" ]; then
                install -m644 "$f" "$out/share/licenses/rgy/$f"
              fi
            done
            runHook postInstall
          '';

          meta = with pkgs.lib; {
            description = "Whole-filesystem name search and content search in one stack";
            longDescription = ''
              rgy indexes the names on your disks once, then answers which
              file matches and what is inside it in a single query. A name
              query is ranked and typo-tolerant; content queries read through
              rgx's engine. It ships a CLI (searchctl), a daemon that keeps
              the index warm (searchd), a launcher (rgxy), a login-item tool
              (rgy-autostart) and the rgy alias for the CLI.

              This package installs the binaries published on the release;
              the source lives at the homepage.
            '';
            homepage = "https://github.com/danalec/rgy";
            changelog = "https://github.com/danalec/rgy/releases/tag/rgy-v${version}";
            license = licenses.asl20;
            mainProgram = "searchctl";
            sourceProvenance = [ sourceTypes.binaryNativeCode ];
            platforms = platforms.unix;
          };
        };

      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          mk = a: mkRgy pkgs a.file a.hash;
          portable = mk assets.${pkgs.stdenv.hostPlatform.system};
          tunedHere =
            if builtins.hasAttr pkgs.stdenv.hostPlatform.system tuned then
              builtins.mapAttrs (_: a: mkRgy pkgs a.file a.hash) tuned.${pkgs.stdenv.hostPlatform.system}
            else
              { };
        in
        {
          rgy-search = portable;
          default = portable;
        }
        // tunedHere
      );

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.default}/bin/searchctl";
          meta.description = "Whole-filesystem name search and content search in one stack";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            rustfmt
            rust-analyzer
            clippy
            cargo-nextest
            mold
            pkg-config
          ];
        };
      });

      checks = forAllSystems (
        pkgs:
        let
          rgy = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        in
        {
          smoke = pkgs.runCommand "rgy-smoke" { nativeBuildInputs = [ rgy ]; } ''
            export HOME=$TMPDIR
            searchctl --version
            rgy --version
            mkdir -p "$TMPDIR/tree"
            printf 'hello from nix\n' > "$TMPDIR/tree/sample.txt"
            searchctl --root "$TMPDIR/tree" --count sample | grep -qx '1'
            touch "$out"
          '';
        }
      );
    };
}
