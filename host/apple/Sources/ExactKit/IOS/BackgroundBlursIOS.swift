// @ref LLP 1123 — the blurred pictures an image's background extension draws
// (`ExtensionView`), kept with the image while it holds its picture: an
// extension view goes when the image leaves the window, and the one made when
// it comes back (a page under a pushed one, shown again by Back) draws the
// blurs already made. A picture shown at another size (pulled past a
// scroller's top, which stretches a header's picture) or decoded again at
// another resolution keeps drawing the blurs made for it, scaled with it,
// while those for its new size are made off the main thread: made on it, they
// took 6 to 8 ms of a frame each time a pull passed the tolerance. A picture
// with no blurs yet, a different picture, a decode of another shape (an SVG
// placed anew) or an animated image's next frame is blurred at once, so a
// mirror is never drawn unblurred or from other content.
#if os(iOS) || os(tvOS)
import CoreImage
import UIKit

final class ExtensionBlurs {
    /// Blurs made of `source`, from `load` (the image's load that gave it, `NodeView.rasterLoad`: another decode of the
    /// same picture keeps it; nil for an animated image's frame, which nothing else stands in for), each by its sigma in
    /// the source's pixels
    struct Made {
        let source: CGImage, load: Int?, sigmas: [CGFloat], images: [CGImage]
    }
    private(set) var made: Made?
    /// What the extension last asked for, which blurs made off the main thread are checked against as they land
    private var wanted: (source: CGImage, load: Int?, sigmas: [CGFloat])?
    /// The blurs being made off the main thread, one at a time
    private var job: Job? { didSet { Self.inFlight += (job == nil ? 0 : 1) - (oldValue == nil ? 0 : 1) } }
    /// Blurs being made off the main thread for every image: the agent's `clock settle` waits for them, as a mirror
    /// changes when they land (`Agent.nativeInFlight`)
    private(set) static var inFlight = 0
    /// Shows blurs made off the main thread (`ExtensionView`)
    var landed: (() -> Void)?

    deinit {
        if job != nil { Self.inFlight -= 1 }
        job?.cancel()
    }

    /// Asks for the blurs of `source` by `sigmas`. Those made stand while their source is the same and each sigma is
    /// within a quarter of the one asked for; those of the same picture (its load, at the same shape) stand while new
    /// ones are made off the main thread (`landed`); else they are made at once, and any being made are cancelled.
    func want(_ source: CGImage, load: Int?, sigmas: [CGFloat]) {
        wanted = (source, load, sigmas)
        if let made, Self.serves(made, source, sigmas) { return }
        if let made, let load, made.load == load, Self.sameShape(made.source, source) { return makeLatest() }
        cancel()
        made = Made(source: source, load: load, sigmas: sigmas, images: Self.blurs(source, sigmas))
    }

    /// The image's blurs went: those being made let their source go, and land nowhere.
    func cancel() {
        job?.cancel()
        job = nil
    }

    private func makeLatest() {
        // A call while blurs are made loses nothing: `wanted` holds it, and they make it when they land
        guard job == nil, let asked = wanted else { return }
        let job = Job(asked.source, load: asked.load, sigmas: asked.sigmas)
        self.job = job
        Self.queue.async { [weak self] in
            guard self != nil, let source = job.source else { return }
            let images = Self.blurs(source, job.sigmas)
            DispatchQueue.main.async {
                guard let self, self.job === job else { return }
                self.job = nil
                // Nothing more while those made serve what is asked now: the picture came back to their size
                guard let now = self.wanted, let made = self.made, !Self.serves(made, now.source, now.sigmas) else { return }
                // These are kept while of the picture made: one blurred at once meanwhile supersedes them
                if job.load == made.load {
                    let kept = Made(source: source, load: job.load, sigmas: job.sigmas, images: images)
                    self.made = kept
                    self.landed?()
                    if Self.serves(kept, now.source, now.sigmas) { return }
                }
                // Asked for another size or decode of the picture while these were made: make that
                self.makeLatest()
            }
        }
    }

    /// Blurs on the queue. Their source goes when they are cancelled, so pixels the raster budget took back are not
    /// held by work waiting to be skipped.
    private final class Job: @unchecked Sendable {
        let load: Int?, sigmas: [CGFloat]
        private let lock = NSLock()
        private var held: CGImage?
        init(_ source: CGImage, load: Int?, sigmas: [CGFloat]) { (held, self.load, self.sigmas) = (source, load, sigmas) }
        /// The source, nil once cancelled: read on the queue, cancelled on the main thread
        var source: CGImage? { lock.lock(); defer { lock.unlock() }; return held }
        func cancel() { lock.lock(); held = nil; lock.unlock() }
    }

    /// Whether two decodes of a load show its picture at another resolution: they have one shape. An SVG drawn into a
    /// box of another shape under `object-fit: fill` is placed anew.
    private static func sameShape(_ a: CGImage, _ b: CGImage) -> Bool {
        let (across, down) = (CGFloat(a.width) * CGFloat(b.height), CGFloat(b.width) * CGFloat(a.height))
        return abs(across - down) <= 0.02 * max(across, down)
    }

    /// Whether blurs made serve `source` by `sigmas`: the same source, each sigma within a quarter of the one asked for
    private static func serves(_ made: Made, _ source: CGImage, _ sigmas: [CGFloat]) -> Bool {
        made.source === source && made.sigmas.count == sigmas.count && zip(made.sigmas, sigmas).allSatisfy { abs($0 - $1) <= 0.25 * $1 }
    }

    private static let queue = DispatchQueue(label: "exact.background-extension-blurs", qos: .userInitiated)

    /// Starts making the blurs' context on their queue once an image that extends its background mounts, before its
    /// picture arrives: made on the main thread for the launch's first picture, it took 9 to 11 ms there on the iPhone
    /// Duo simulator.
    static func warm() { _ = warming }
    private static let warming: Void = queue.async { _ = context }

    /// Blurs the pixels' encoded values, as UIKit's blur does: blurred in linear light, a detailed picture's mirror came
    /// out lighter than the picture. A context is safe to share across threads.
    private static let context = CIContext(options: [.cacheIntermediates: false, .workingColorSpace: NSNull(), .outputColorSpace: NSNull()])

    /// One render per level: the levels rendered into one image and cut out made the same pixels in 3.1 ms instead of
    /// 5.3 on an M5 Pro Mac and no faster on the iPhone Duo simulator.
    private static func blurs(_ source: CGImage, _ sigmas: [CGFloat]) -> [CGImage] {
        sigmas.map { blur(source, sigma: $0) ?? source }
    }

    /// `image` blurred by `sigma` of its pixels, made no larger than that blur needs: at most four of its pixels to
    /// each of the blur's sigma, which the layer draws at the picture's size.
    private static func blur(_ image: CGImage, sigma: CGFloat) -> CGImage? {
        let scale = min(1, 4 / max(sigma, 0.001))
        var input = CIImage(cgImage: image)
        if scale < 1 { input = input.transformed(by: CGAffineTransform(scaleX: scale, y: scale)) }
        let extent = input.extent.integral
        let output = input.clampedToExtent().applyingGaussianBlur(sigma: Double(sigma * scale)).cropped(to: extent)
        // Tagged with the picture's own colour space, its values untouched
        return context.createCGImage(output, from: extent, format: .RGBA8, colorSpace: image.colorSpace ?? CGColorSpace(name: CGColorSpace.sRGB)!)
    }
}
#endif
