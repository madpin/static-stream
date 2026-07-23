import Foundation

private enum InstallerError: LocalizedError {
    case bundledDriverMissing(String)
    case unsupportedCommand(String)
    case authorizationFailed(String)

    var errorDescription: String? {
        switch self {
        case .bundledDriverMissing(let path):
            "The bundled audio driver is missing at \(path)."
        case .unsupportedCommand(let command):
            "Unsupported audio driver command: \(command)"
        case .authorizationFailed(let message):
            message
        }
    }
}

private func shellQuote(_ value: String) -> String {
    "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
}

private func runAuthorized(_ command: String) throws {
    let escaped = command
        .replacingOccurrences(of: "\\", with: "\\\\")
        .replacingOccurrences(of: "\"", with: "\\\"")
    let process = Process()
    process.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
    process.arguments = [
        "-e",
        "do shell script \"\(escaped)\" with administrator privileges",
    ]
    let errorPipe = Pipe()
    process.standardError = errorPipe
    try process.run()
    process.waitUntilExit()
    guard process.terminationStatus == 0 else {
        let data = errorPipe.fileHandleForReading.readDataToEndOfFile()
        let detail = String(decoding: data, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        throw InstallerError.authorizationFailed(
            detail.isEmpty ? "The Static Microphone change was cancelled." : detail
        )
    }
}

@main
enum StaticStreamAudioDriverInstaller {
    static func main() {
        do {
            let command = CommandLine.arguments.dropFirst().first ?? "install"
            let executable = URL(fileURLWithPath: CommandLine.arguments[0])
                .standardizedFileURL
            let contents = executable
                .deletingLastPathComponent()
                .deletingLastPathComponent()
            let source = contents
                .appendingPathComponent("Resources", isDirectory: true)
                .appendingPathComponent("StaticStreamAudio.driver", isDirectory: true)
            let destination = "/Library/Audio/Plug-Ins/HAL/StaticStreamAudio.driver"
            let ownedDestinations = [
                destination,
                "/Library/Audio/Plug-Ins/HAL/Static Stream Audio.driver",
                "/Library/Audio/Plug-Ins/HAL/StaticStream.driver",
            ]
            let removeOwnedDrivers = ownedDestinations
                .map { "/bin/rm -rf \(shellQuote($0))" }
                .joined(separator: " && ")

            switch command {
            case "install":
                guard FileManager.default.fileExists(atPath: source.path) else {
                    throw InstallerError.bundledDriverMissing(source.path)
                }
                let script = [
                    "/bin/mkdir -p /Library/Audio/Plug-Ins/HAL",
                    removeOwnedDrivers,
                    "/usr/bin/ditto \(shellQuote(source.path)) \(shellQuote(destination))",
                    "/usr/sbin/chown -R root:wheel \(shellQuote(destination))",
                    "/bin/chmod -R a+rX \(shellQuote(destination))",
                    "( /usr/bin/killall coreaudiod >/dev/null 2>&1 || true )",
                ].joined(separator: " && ")
                try runAuthorized(script)
                print("Static Microphone installed.")
            case "uninstall":
                try runAuthorized(
                    removeOwnedDrivers + " && " +
                    "( /usr/bin/killall coreaudiod >/dev/null 2>&1 || true )"
                )
                print("Static Microphone and older Static Stream audio drivers removed.")
            default:
                throw InstallerError.unsupportedCommand(command)
            }
        } catch {
            fputs("\(error.localizedDescription)\n", stderr)
            exit(EXIT_FAILURE)
        }
    }
}
