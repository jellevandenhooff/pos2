{
  description = "A very basic flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
    naersk.url = "github:nix-community/naersk";
  };

  outputs = { self, nixpkgs, naersk, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rustVersion = pkgs.rust-bin.stable.latest.default;

        naersk' = pkgs.callPackage naersk {
          cargo = rustVersion;
          rustc = rustVersion;
        };

        tunnelPackage = naersk'.buildPackage {
          name =
            "tunnel"; # make this what ever your cargo.toml package.name is
          version = "0.1.0";
          src = ./tunnel; # the folder with the cargo.toml

          nativeBuildInputs = [pkgs.pkg-config]; # just for the host building the package
          buildInputs = [pkgs.openssl]; # packages needed by the consumer

          doCheck = false; # TODO fix tests
        };

        tunnelEnv = pkgs.buildEnv {
            name = "tunnel";
            paths = [ tunnelPackage ];
            pathsToLink = [ "/bin" ];
        };

        tunnelImage = pkgs.dockerTools.buildImage {
          name = "tunnel-image";
          config = { Cmd = [ "tunnel" ]; };

          # created = "now";
          copyToRoot = with pkgs.dockerTools; [
            tunnelEnv
            caCertificates
            usrBinEnv
            binSh
            fakeNss
            pkgs.coreutils
          ];
        };

        loaderPackage = naersk'.buildPackage {
          name =
            "loader"; # make this what ever your cargo.toml package.name is
          version = "0.1.0";
          src = ./loader; # the folder with the cargo.toml

          nativeBuildInputs = [pkgs.pkg-config]; # just for the host building the package
          buildInputs = [pkgs.openssl]; # packages needed by the consumer

          doCheck = false; # TODO fix tests
        };

        loaderEnv = pkgs.buildEnv {
            name = "loader";
            paths = [ loaderPackage ];
            pathsToLink = [ "/bin" ];
        };

        loaderImage = pkgs.dockerTools.buildImage {
          name = "loader-image";
          config = { Cmd = [ "loader" ]; };

          # created = "now";
          copyToRoot = with pkgs.dockerTools; [
            loaderEnv
            caCertificates
            usrBinEnv
            binSh
            fakeNss
            pkgs.coreutils
          ];
        };

        innerappPackage = naersk'.buildPackage {
          name =
            "innerapp"; # make this what ever your cargo.toml package.name is
          version = "0.1.0";
          src = ./innerapp; # the folder with the cargo.toml

          nativeBuildInputs = [pkgs.pkg-config]; # just for the host building the package
          buildInputs = [pkgs.openssl]; # packages needed by the consumer

          doCheck = false; # TODO fix tests
        };

        innerappPlainImage = pkgs.dockerTools.buildImage {
          name = "innerapp-image";
          config = { Cmd = [ "${innerappPackage}/bin/innerapp" ]; };
        };

      in {
        packages = {
          # tunnelPackage = tunnelPackage;
          tunnelImage = tunnelImage;
          loaderImage = loaderImage;
          innerappPlainImage = innerappPlainImage;
        };
        defaultPackage = tunnelImage;
        # devShell = pkgs.mkShell {
          # buildInputs =
            # [ (rustVersion.override { extensions = [ "rust-src" ]; }) ];
        # };
      }
    );
}
