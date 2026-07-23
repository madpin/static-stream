struct FreezeFrameSelector<Frame> {
    private var heldFrame: Frame?

    mutating func select(currentFrame: Frame, frozen: Bool) -> Frame {
        if !frozen || heldFrame == nil {
            heldFrame = currentFrame
        }
        return frozen ? heldFrame! : currentFrame
    }

    mutating func reset() {
        heldFrame = nil
    }
}
