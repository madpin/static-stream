import CoreMediaIO
import Foundation

let providerSource = StaticStreamCameraProviderSource()
CMIOExtensionProvider.startService(provider: providerSource.provider)
CFRunLoopRun()

