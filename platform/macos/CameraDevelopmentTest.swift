import AVFoundation
import Foundation

@main
enum StaticStreamCameraDevelopmentTest {
    static func main() {
        var selector = FreezeFrameSelector<Int>()

        guard selector.select(currentFrame: 1, frozen: false) == 1,
              selector.select(currentFrame: 2, frozen: false) == 2,
              selector.select(currentFrame: 3, frozen: true) == 2,
              selector.select(currentFrame: 4, frozen: true) == 2
        else {
            fputs("Camera freeze test failed while holding a frame.\n", stderr)
            exit(EXIT_FAILURE)
        }

        guard selector.select(currentFrame: 5, frozen: false) == 5 else {
            fputs("Camera freeze test failed while returning to live video.\n", stderr)
            exit(EXIT_FAILURE)
        }

        selector.reset()
        guard selector.select(currentFrame: 6, frozen: true) == 6 else {
            fputs("Camera freeze test failed after resetting the stream.\n", stderr)
            exit(EXIT_FAILURE)
        }

        let names = physicalCameraNames()
        let cameraSummary = names.isEmpty
            ? "No physical camera is currently discoverable."
            : "Physical camera(s): \(names.joined(separator: ", "))."
        print(
            "Unsigned camera test passed: live frames advance, Freeze holds the last frame, " +
                "and Live resumes with a new frame. \(cameraSummary)"
        )
    }

    private static func physicalCameraNames() -> [String] {
        var deviceTypes: [AVCaptureDevice.DeviceType] = [.builtInWideAngleCamera]
        if #available(macOS 14.0, *) {
            deviceTypes.append(.external)
        } else {
            deviceTypes.append(.externalUnknown)
        }
        return AVCaptureDevice.DiscoverySession(
            deviceTypes: deviceTypes,
            mediaType: .video,
            position: .unspecified
        )
        .devices
        .filter { $0.localizedName != "Static Camera" }
        .map(\.localizedName)
        .sorted()
    }
}
