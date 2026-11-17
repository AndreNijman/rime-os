package com.rimeos.remote.core.nex

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.doubleOrNull
import kotlinx.serialization.json.longOrNull

// Reading nexd's JSON by hand, field by field.
//
// Not `@Serializable` classes, on purpose: nexd is a separate program that
// will grow fields, turn a number into a float (its token totals already are
// floats) and send null where a value is unknown. A decoder that threw on any
// of those would blank a whole screen over one field nobody looks at. Every
// accessor here answers null for "absent, null or the wrong type", and each
// caller decides what a missing value means.

internal fun JsonObject.str(key: String): String? =
    (this[key] as? JsonPrimitive)?.takeIf { it.isString }?.content

internal fun JsonObject.long(key: String): Long? =
    (this[key] as? JsonPrimitive)?.takeIf { !it.isString }?.let { it.longOrNull ?: it.doubleOrNull?.toLong() }

internal fun JsonObject.double(key: String): Double? =
    (this[key] as? JsonPrimitive)?.takeIf { !it.isString }?.doubleOrNull

internal fun JsonObject.bool(key: String): Boolean? =
    (this[key] as? JsonPrimitive)?.takeIf { !it.isString }?.booleanOrNull

internal fun JsonObject.obj(key: String): JsonObject? = this[key] as? JsonObject

internal fun JsonObject.objects(key: String): List<JsonObject> =
    (this[key] as? JsonArray)?.filterIsInstance<JsonObject>() ?: emptyList()

internal fun JsonObject.strings(key: String): List<String> =
    (this[key] as? JsonArray)?.mapNotNull { (it as? JsonPrimitive)?.takeIf { p -> p.isString }?.content }
        ?: emptyList()

internal fun JsonElement?.objects(): List<JsonObject> =
    (this as? JsonArray)?.filterIsInstance<JsonObject>() ?: emptyList()

/** A short, one-line rendering of tool arguments, for a row that has one line. */
internal fun JsonElement?.oneLine(max: Int = 160): String {
    val text = when (this) {
        null, JsonNull -> ""
        is JsonPrimitive -> content
        is JsonObject -> entries.joinToString("  ") { (k, v) ->
            "$k: " + if (v is JsonPrimitive) v.content else v.toString()
        }
        else -> toString()
    }
    val flat = text.replace(Regex("\\s+"), " ").trim()
    return if (flat.length <= max) flat else flat.take(max - 1) + "…"
}
