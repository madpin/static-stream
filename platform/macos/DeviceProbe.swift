import AVFoundation
import Foundation

private struct DeviceStatus: Encodable {
    let cameraNames: [String]
    let staticStreamCameraAvailable: Bool
}

@main
enum StaticStreamDeviceProbe {
    static func main() throws {
        let deviceTypes: [AVCaptureDevice.DeviceType]
        if #available(macOS 14.0, *) {
            deviceTypes = [.builtInWideAngleCamera, .external]
        } else {
            deviceTypes = [.builtInWideAngleCamera, .externalUnknown]
        }
        let cameras = AVCaptureDevice.DiscoverySession(
            deviceTypes: deviceTypes,
            mediaType: .video,
            position: .unspecified
        )
        .devices
        .map(\.localizedName)
        .sorted()

        let status = DeviceStatus(
            cameraNames: cameras,
            staticStreamCameraAvailable: cameras.contains("Static Camera")
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        FileHandle.standardOutput.write(try encoder.encode(status))
        FileHandle.standardOutput.write(Data([0x0A]))
    }
}
