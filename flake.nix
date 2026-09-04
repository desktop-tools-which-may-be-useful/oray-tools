{
  description = "oray-tools: CLI tool for Oray smart plug control";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/1559d3daa3ecc813a650b79375ea61b6741b8746";
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
            version = "1.0.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            meta = with pkgs.lib; {
              description = "Oray (Sunlogin) device control CLI";
              longDescription = ''
                oray-tools provides command-line control of Oray (Sunlogin)
                devices through the cloud APIs:
                - auth login/refresh/status/logout: manage locally stored tokens
                - wakeup list/info/rename/memo: 开机设备 (smart plugs, power
                  hardware), fetched live from the cloud
                - wakeup plug status/on/off/logs/timer/countdown/led/
                  power-on-restore: smart-plug controls
                - remote list/info/status/rename/memo: 远程设备 (PCs, phones)
                - --json / --verbose output on every command, plus
                  --refresh-on-expired to auto-refresh tokens on TOKEN_EXPIRED
                Config is stored in $XDG_CONFIG_HOME/oray-tools/config.toml.
              '';
              homepage = "https://github.com/desktop-tools-which-may-be-useful/oray-tools";
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
