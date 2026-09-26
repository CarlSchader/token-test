{
  description = "token-test: LLM load-testing CLI";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "token-test";
          version = "0.1.0";
          src = self;

          # Pin to the committed lock file for reproducible builds.
          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          meta = with pkgs.lib; {
            description = "Load-test OpenAI-compatible LLM servers: measure tokens/s and throughput under concurrent load.";
            license = licenses.mit;
            mainProgram = "token-test";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.rustc
            pkgs.cargo
          ];
        };
      });
}
