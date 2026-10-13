package com.apexos.remote.pairing

import androidx.camera.core.ImageProxy
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.MultiFormatReader
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.common.HybridBinarizer

/**
 * Turning camera frames into a pairing payload.
 *
 * ZXing's `core` artifact only — the `android-core` and `android-integration`
 * helpers bring an Activity and a layout with them, and this app owns its own
 * camera surface. ML Kit was the alternative and is not used: it is a Play
 * Services dependency, and an OS that ships its own app store story should not
 * make its remote client refuse to scan a QR code on a phone without Google's
 * services on it.
 */
class QrScanner {
    private val reader = MultiFormatReader().apply {
        setHints(
            mapOf(
                // QR only. A pairing payload is never a barcode, and every
                // other format is a format the decoder spends time failing at
                // on sixty frames a second.
                DecodeHintType.POSSIBLE_FORMATS to listOf(com.google.zxing.BarcodeFormat.QR_CODE),
                // The desktop's code is on a screen and is big and clean, so
                // the slow, forgiving path buys nothing and costs frames.
                DecodeHintType.TRY_HARDER to false,
            ),
        )
    }

    /**
     * Decode one frame, or `null`.
     *
     * The luminance plane is read with its **row stride**, not its width. They
     * differ on most devices — the camera pads rows to an alignment — and a
     * decoder that assumed they were equal reads a sheared image that never
     * decodes, on exactly the hardware nobody tested on.
     */
    fun decode(image: ImageProxy): String? {
        val plane = image.planes[0]
        val buffer = plane.buffer
        val rowStride = plane.rowStride
        val width = image.width
        val height = image.height
        val bytes = ByteArray(rowStride * height)
        buffer.rewind()
        buffer.get(bytes, 0, minOf(bytes.size, buffer.remaining()))
        val source = PlanarYUVLuminanceSource(
            bytes,
            rowStride,
            height,
            0,
            0,
            width,
            height,
            false,
        )
        return try {
            reader.decodeWithState(BinaryBitmap(HybridBinarizer(source))).text
        } catch (_: NotFoundException) {
            // The overwhelmingly common case: this frame had no code in it.
            null
        } finally {
            // `decodeWithState` keeps per-frame state that must not survive
            // into the next one.
            reader.reset()
        }
    }
}
