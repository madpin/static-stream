import Foundation
import SystemExtensions

private let extensionIdentifier = "com.madpin.staticstream.camera"

private enum Operation: String {
    case install
    case uninstall
}

private final class ActivationDelegate: NSObject, OSSystemExtensionRequestDelegate {
    private let operation: Operation

    init(operation: Operation) {
        self.operation = operation
        super.init()
    }

    func request(
        _ request: OSSystemExtensionRequest,
        actionForReplacingExtension existing: OSSystemExtensionProperties,
        withExtension ext: OSSystemExtensionProperties
    ) -> OSSystemExtensionRequest.ReplacementAction {
        .replace
    }

    func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
        let action = operation == .install ? "installation" : "removal"
        fputs("Static Camera \(action) needs approval in System Settings.\n", stderr)
    }

    func request(
        _ request: OSSystemExtensionRequest,
        didFinishWithResult result: OSSystemExtensionRequest.Result
    ) {
        if operation == .install {
            print(
                result == .completed
                    ? "Static Camera installed."
                    : "Static Camera installation will finish after restarting macOS."
            )
        } else {
            print(
                result == .completed
                    ? "Static Camera removed."
                    : "Static Camera removal will finish after restarting macOS."
            )
        }
        exit(EXIT_SUCCESS)
    }

    func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
        let action = operation == .install ? "installation" : "removal"
        fputs("Static Camera \(action) failed: \(error.localizedDescription)\n", stderr)
        exit(EXIT_FAILURE)
    }
}

@main
enum StaticStreamActivationHelper {
    static func main() {
        let argument = CommandLine.arguments.dropFirst().first ?? Operation.install.rawValue
        guard let operation = Operation(rawValue: argument) else {
            fputs("Unsupported camera command: \(argument)\n", stderr)
            exit(EXIT_FAILURE)
        }
        let delegate = ActivationDelegate(operation: operation)
        let request: OSSystemExtensionRequest
        switch operation {
        case .install:
            request = OSSystemExtensionRequest.activationRequest(
                forExtensionWithIdentifier: extensionIdentifier,
                queue: .main
            )
        case .uninstall:
            request = OSSystemExtensionRequest.deactivationRequest(
                forExtensionWithIdentifier: extensionIdentifier,
                queue: .main
            )
        }
        request.delegate = delegate
        OSSystemExtensionManager.shared.submitRequest(request)
        RunLoop.main.run()
    }
}
