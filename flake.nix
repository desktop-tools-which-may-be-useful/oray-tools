{
  description = "oray-tools: CLI tool for Oray smart plug control";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/e7a3ca8092b61ff85b6a45bf863ea2b2d6a661b3";
  };

  outputs =
    { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "oray-tools";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            meta = with pkgs.lib; {
              description = "Oray smart plug control CLI";
              longDescription = ''
                oray-tools provides command-line control of Oray smart plugs:
                - login: authenticate and persist tokens (supports the SMS
                  verification flow used when registering a new trusted device)
                - refresh: renew tokens using refresh_token
                - status / on / off: query and switch a plug (auto-refreshes
                  when the access token is expired)
                - tokens / logout: show token info / clear saved state
                Config is stored in $XDG_CONFIG_HOME/oray-tools/config.toml.
              '';
              homepage = "https://github.com/anomalyco/opencode";
              license = licenses.mit;
              platforms = platforms.linux;
              mainProgram = "oray-tools";
            };
          };
        }
      );

      apps = forAllSystems (
        system:
        {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/oray-tools";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [ cargo rustc ];
          };
        }
      );
    };
}
