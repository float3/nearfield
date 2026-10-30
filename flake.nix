{
  description = "Daily arXiv preprints sorted by how far each paper's nearest author is from a place you choose";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = {
    self,
    nixpkgs,
  }: let
    lib = nixpkgs.lib;
    systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
    forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
  in {
    formatter = forAllSystems (pkgs: pkgs.alejandra);

    packages = forAllSystems (pkgs: {
      default = pkgs.rustPlatform.buildRustPackage {
        pname = "nearfield";
        version = (lib.importTOML ./Cargo.toml).workspace.package.version;
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [./Cargo.toml ./Cargo.lock ./crates];
        };
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = ["-p" "nearfield-cli"];
        cargoTestFlags = ["-p" "nearfield-cli" "-p" "nearfield-core"];
        meta.mainProgram = "nearfield";
      };
    });

    devShells = forAllSystems (pkgs: {
      default = pkgs.mkShell {
        packages = with pkgs; [cargo clippy rustc rustfmt rust-analyzer nodejs_22];
      };
    });
  };
}
