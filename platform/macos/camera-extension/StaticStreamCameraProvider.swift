import AVFoundation
import CoreMediaIO
import Foundation
import IOKit.audio
import os.log

private let frameRate: Int32 = 30
private let frameWidth: Int32 = 1280
private let frameHeight: Int32 = 720
private let appGroup = "__APP_GROUP__"
private let frozenKey = "cameraFrozen"

final class StaticStreamCameraDeviceSource: NSObject, CMIOExtensionDeviceSource,
    AVCaptureVideoDataOutputSampleBufferDelegate, @unchecked Sendable
{
    private(set) var device: CMIOExtensionDevice!
    private var streamSource: StaticStreamCameraStreamSource!

    private let captureSession = AVCaptureSession()
    private let captureQueue = DispatchQueue(
        label: "com.madpin.staticstream.camera.capture",
        qos: .userInteractive
    )
    private let defaults = UserDefaults(suiteName: appGroup)
    private var streamingClients = 0
    private var frameSelector = FreezeFrameSelector<CVPixelBuffer>()
    private var videoDescription: CMFormatDescription!

    init(localizedName: String) {
        super.init()

        let deviceID = UUID(uuidString: "2A84EB7E-0A6C-4CF2-963B-5F1DDB4218AF")!
        device = CMIOExtensionDevice(
            localizedName: localizedName,
            deviceID: deviceID,
            legacyDeviceID: nil,
            source: self
        )

        let status = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            codecType: kCVPixelFormatType_32BGRA,
            width: frameWidth,
            height: frameHeight,
            extensions: nil,
            formatDescriptionOut: &videoDescription
        )
        precondition(status == noErr, "Unable to create camera format: \(status)")

        let streamFormat = CMIOExtensionStreamFormat(
            formatDescription: videoDescription,
            maxFrameDuration: CMTime(value: 1, timescale: frameRate),
            minFrameDuration: CMTime(value: 1, timescale: frameRate),
            validFrameDurations: nil
        )
        let streamID = UUID(uuidString: "47F8A16C-7DA1-4FC9-95DD-859865FDD913")!
        streamSource = StaticStreamCameraStreamSource(
            localizedName: "Static Video",
            streamID: streamID,
            streamFormat: streamFormat,
            device: device
        )
        do {
            try device.addStream(streamSource.stream)
        } catch {
            fatalError("Unable to publish camera stream: \(error.localizedDescription)")
        }

        configureCaptureSession()
    }

    var availableProperties: Set<CMIOExtensionProperty> {
        [.deviceTransportType, .deviceModel]
    }

    func deviceProperties(
        forProperties properties: Set<CMIOExtensionProperty>
    ) throws -> CMIOExtensionDeviceProperties {
        let result = CMIOExtensionDeviceProperties(dictionary: [:])
        if properties.contains(.deviceTransportType) {
            result.transportType = kIOAudioDeviceTransportTypeVirtual
        }
        if properties.contains(.deviceModel) {
            result.model = "Static Stream Virtual Camera"
        }
        return result
    }

    func setDeviceProperties(_ deviceProperties: CMIOExtensionDeviceProperties) throws {}

    func startStreaming() {
        captureQueue.async { [self] in
            streamingClients += 1
            if streamingClients == 1, !captureSession.isRunning {
                captureSession.startRunning()
            }
        }
    }

    func stopStreaming() {
        captureQueue.async { [self] in
            streamingClients = max(0, streamingClients - 1)
            if streamingClients == 0, captureSession.isRunning {
                captureSession.stopRunning()
                frameSelector.reset()
            }
        }
    }

    func captureOutput(
        _ output: AVCaptureOutput,
        didOutput sampleBuffer: CMSampleBuffer,
        from connection: AVCaptureConnection
    ) {
        guard let currentBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else {
            return
        }

        let frozen = defaults?.bool(forKey: frozenKey) ?? false
        let selectedBuffer = frameSelector.select(currentFrame: currentBuffer, frozen: frozen)
        send(pixelBuffer: selectedBuffer)
    }

    private func configureCaptureSession() {
        captureSession.beginConfiguration()
        captureSession.sessionPreset = .hd1280x720
        defer { captureSession.commitConfiguration() }

        var deviceTypes: [AVCaptureDevice.DeviceType] = [.builtInWideAngleCamera]
        if #available(macOS 14.0, *) {
            deviceTypes.append(.external)
        } else {
            deviceTypes.append(.externalUnknown)
        }
        let discovery = AVCaptureDevice.DiscoverySession(
            deviceTypes: deviceTypes,
            mediaType: .video,
            position: .unspecified
        )
        let physicalCamera = discovery.devices.first {
            $0.localizedName != "Static Camera"
        }
        guard let physicalCamera else {
            os_log(.error, "Static Stream could not find a physical camera")
            return
        }

        do {
            let input = try AVCaptureDeviceInput(device: physicalCamera)
            guard captureSession.canAddInput(input) else {
                os_log(.error, "Static Stream cannot use %{public}@", physicalCamera.localizedName)
                return
            }
            captureSession.addInput(input)
        } catch {
            os_log(.error, "Static Stream camera input failed: %{public}@", error.localizedDescription)
            return
        }

        let output = AVCaptureVideoDataOutput()
        output.alwaysDiscardsLateVideoFrames = true
        output.videoSettings = [
            kCVPixelBufferPixelFormatTypeKey as String: Int(kCVPixelFormatType_32BGRA),
            kCVPixelBufferWidthKey as String: Int(frameWidth),
            kCVPixelBufferHeightKey as String: Int(frameHeight),
        ]
        output.setSampleBufferDelegate(self, queue: captureQueue)
        guard captureSession.canAddOutput(output) else {
            os_log(.error, "Static Stream cannot create a camera output")
            return
        }
        captureSession.addOutput(output)

    }

    private func send(pixelBuffer: CVPixelBuffer) {
        let presentationTime = CMClockGetTime(CMClockGetHostTimeClock())
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: frameRate),
            presentationTimeStamp: presentationTime,
            decodeTimeStamp: .invalid
        )
        var outputBuffer: CMSampleBuffer?
        let status = CMSampleBufferCreateReadyWithImageBuffer(
            allocator: kCFAllocatorDefault,
            imageBuffer: pixelBuffer,
            formatDescription: videoDescription,
            sampleTiming: &timing,
            sampleBufferOut: &outputBuffer
        )
        guard status == noErr, let outputBuffer else {
            os_log(.error, "Static Stream failed to create a frame: %d", status)
            return
        }

        let hostTime = UInt64(max(0, presentationTime.seconds) * Double(NSEC_PER_SEC))
        streamSource.stream.send(outputBuffer, discontinuity: [], hostTimeInNanoseconds: hostTime)
    }
}

final class StaticStreamCameraStreamSource: NSObject, CMIOExtensionStreamSource {
    private(set) var stream: CMIOExtensionStream!
    let device: CMIOExtensionDevice
    private let streamFormat: CMIOExtensionStreamFormat

    init(
        localizedName: String,
        streamID: UUID,
        streamFormat: CMIOExtensionStreamFormat,
        device: CMIOExtensionDevice
    ) {
        self.device = device
        self.streamFormat = streamFormat
        super.init()
        stream = CMIOExtensionStream(
            localizedName: localizedName,
            streamID: streamID,
            direction: .source,
            clockType: .hostTime,
            source: self
        )
    }

    var formats: [CMIOExtensionStreamFormat] {
        [streamFormat]
    }

    var activeFormatIndex: Int = 0 {
        didSet {
            if activeFormatIndex != 0 {
                activeFormatIndex = 0
            }
        }
    }

    var availableProperties: Set<CMIOExtensionProperty> {
        [.streamActiveFormatIndex, .streamFrameDuration]
    }

    func streamProperties(
        forProperties properties: Set<CMIOExtensionProperty>
    ) throws -> CMIOExtensionStreamProperties {
        let result = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) {
            result.activeFormatIndex = 0
        }
        if properties.contains(.streamFrameDuration) {
            result.frameDuration = CMTime(value: 1, timescale: frameRate)
        }
        return result
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {
        if let formatIndex = streamProperties.activeFormatIndex {
            activeFormatIndex = formatIndex
        }
    }

    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        true
    }

    func startStream() throws {
        guard let source = device.source as? StaticStreamCameraDeviceSource else {
            throw StaticStreamCameraError.invalidDeviceSource
        }
        source.startStreaming()
    }

    func stopStream() throws {
        guard let source = device.source as? StaticStreamCameraDeviceSource else {
            throw StaticStreamCameraError.invalidDeviceSource
        }
        source.stopStreaming()
    }
}

final class StaticStreamCameraProviderSource: NSObject, CMIOExtensionProviderSource {
    private(set) var provider: CMIOExtensionProvider!
    private var deviceSource: StaticStreamCameraDeviceSource!

    override init() {
        super.init()
        provider = CMIOExtensionProvider(source: self, clientQueue: nil)
        deviceSource = StaticStreamCameraDeviceSource(localizedName: "Static Camera")
        do {
            try provider.addDevice(deviceSource.device)
        } catch {
            fatalError("Unable to publish Static Camera: \(error.localizedDescription)")
        }
    }

    func connect(to client: CMIOExtensionClient) throws {}
    func disconnect(from client: CMIOExtensionClient) {}

    var availableProperties: Set<CMIOExtensionProperty> {
        [.providerManufacturer]
    }

    func providerProperties(
        forProperties properties: Set<CMIOExtensionProperty>
    ) throws -> CMIOExtensionProviderProperties {
        let result = CMIOExtensionProviderProperties(dictionary: [:])
        if properties.contains(.providerManufacturer) {
            result.manufacturer = "MadPin"
        }
        return result
    }

    func setProviderProperties(_ providerProperties: CMIOExtensionProviderProperties) throws {}
}

private enum StaticStreamCameraError: Error {
    case invalidDeviceSource
}
