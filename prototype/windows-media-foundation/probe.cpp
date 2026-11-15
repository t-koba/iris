// Test-only, unpackaged Windows SDK probe. No Iris runtime or external codec DLLs.
#define NOMINMAX
#include <windows.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <wrl/client.h>

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <limits>
#include <stdexcept>
#include <string>

using Microsoft::WRL::ComPtr;

// Media Foundation expresses sentinel stream indices as signed constants,
// while the Source Reader API accepts DWORD. Preserve their bit patterns.
constexpr DWORD kAllStreams = static_cast<DWORD>(MF_SOURCE_READER_ALL_STREAMS);
constexpr DWORD kCurrentType = static_cast<DWORD>(MF_SOURCE_READER_CURRENT_TYPE_INDEX);
constexpr DWORD kFirstVideoStream = static_cast<DWORD>(MF_SOURCE_READER_FIRST_VIDEO_STREAM);

struct Failure {
    const char* stage;
    HRESULT hr;
    DWORD flags = 0; // Only decode_flags failures retain the bounded DWORD mask.
};

void check(HRESULT hr, const char* stage) {
    if (FAILED(hr)) throw Failure{stage, hr};
}

uint64_t positive(const wchar_t* text) {
    std::wstring value(text);
    if (value.empty() || value.find_first_not_of(L"0123456789") != std::wstring::npos)
        throw Failure{"arguments", E_INVALIDARG};
    auto number = std::stoull(value);
    if (!number) throw Failure{"arguments", E_INVALIDARG};
    return number;
}

struct Limits {
    uint64_t dimension;
    uint64_t bytes;
    uint64_t reads;
};

struct Layout {
    UINT32 width;
    UINT32 height;
    uint64_t stride;
    uint64_t bytes;
};

Layout layout(IMFMediaType* type, const Limits& limits, bool rgb) {
    GUID major, subtype;
    check(type->GetGUID(MF_MT_MAJOR_TYPE, &major), "media_major_type");
    if (major != MFMediaType_Video) throw Failure{"not_video", E_FAIL};
    check(type->GetGUID(MF_MT_SUBTYPE, &subtype), rgb ? "output_subtype" : "native_subtype");
    if (subtype != (rgb ? MFVideoFormat_RGB32 : MFVideoFormat_H264))
        throw Failure{rgb ? "not_rgb32" : "not_h264", E_FAIL};
    UINT32 width = 0, height = 0;
    check(MFGetAttributeSize(type, MF_MT_FRAME_SIZE, &width, &height), "frame_size");
    if (!width || !height || width > limits.dimension || height > limits.dimension)
        throw Failure{"dimension_bound", E_INVALIDARG};
    // width/height are UINT32; guard multiplication before any output allocation.
    const uint64_t row = uint64_t(width) * 4;
    uint64_t stride = row;
    if (rgb) {
        UINT32 encoded = 0;
        const auto hr = type->GetUINT32(MF_MT_DEFAULT_STRIDE, &encoded);
        if (hr == MF_E_ATTRIBUTENOTFOUND) {
            LONG default_stride = 0;
            check(MFGetStrideForBitmapInfoHeader(MFVideoFormat_RGB32.Data1, width, &default_stride), "default_stride");
            encoded = static_cast<UINT32>(default_stride);
        } else {
            check(hr, "output_stride");
        }
        const int64_t signed_stride = static_cast<LONG>(encoded);
        stride = signed_stride < 0 ? uint64_t(-signed_stride) : uint64_t(signed_stride);
    }
    if (stride < row || stride > limits.bytes || height > limits.bytes / stride)
        throw Failure{"output_bound", E_INVALIDARG};
    return {width, height, stride, stride * height};
}

// Both notification kinds require both types to be checked, before using a
// sample from that ReadSample call (or continuing after a notification alone).
Layout checked_output(IMFSourceReader* reader, const Limits& limits, const Layout& input) {
    ComPtr<IMFMediaType> native;
    check(reader->GetNativeMediaType(kFirstVideoStream, kCurrentType, &native), "native_type");
    const auto current_input = layout(native.Get(), limits, false);
    if (current_input.width != input.width || current_input.height != input.height)
        throw Failure{"unexpected_resize", E_FAIL};
    ComPtr<IMFMediaType> output;
    check(reader->GetCurrentMediaType(kFirstVideoStream, &output), "output_type");
    const auto frame = layout(output.Get(), limits, true);
    if (frame.width != input.width || frame.height != input.height)
        throw Failure{"unexpected_resize", E_FAIL};
    return frame;
}

void validate_read(IMFSourceReader* reader, DWORD flags, const Limits& limits,
                   const Layout& input, Layout& frame) {
    constexpr DWORD changes = MF_SOURCE_READERF_NATIVEMEDIATYPECHANGED |
                              MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED;
    // A stream tick can carry no sample. Everything else, including ERROR,
    // ENDOFSTREAM, NEWSTREAM and unknown bits, is fatal before the first frame.
    constexpr DWORD allowed = changes | MF_SOURCE_READERF_STREAMTICK;
    if (flags & ~allowed) throw Failure{"decode_flags", E_FAIL, flags};
    if (flags & changes) frame = checked_output(reader, limits, input);
}

struct Runtime {
    bool com = false;
    bool mf = false;
    ~Runtime() {
        if (mf) MFShutdown();
        if (com) CoUninitialize();
    }
};

struct LockedBuffer {
    IMFMediaBuffer* buffer;
    BYTE* data = nullptr;
    DWORD capacity = 0, length = 0;
    explicit LockedBuffer(IMFMediaBuffer* value) : buffer(value) {
        check(buffer->Lock(&data, &capacity, &length), "buffer_lock");
    }
    ~LockedBuffer() { buffer->Unlock(); }
};

struct Result {
    Layout frame;
    DWORD bytes;
    uint64_t reads;
    uint64_t checksum;
    LONGLONG timestamp;
};

Result decode(const wchar_t* path, const Limits& limits) {
    Runtime runtime;
    check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "com_startup");
    runtime.com = true;
    check(MFStartup(MF_VERSION, MFSTARTUP_FULL), "mf_startup");
    runtime.mf = true;
    ComPtr<IMFAttributes> attributes;
    check(MFCreateAttributes(&attributes, 1), "attributes");
    // Source Reader's system video processor converts H.264 output to RGB32.
    check(attributes->SetUINT32(MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, TRUE), "video_processing");
    ComPtr<IMFSourceReader> reader;
    check(MFCreateSourceReaderFromURL(path, attributes.Get(), &reader), "source_open");
    check(reader->SetStreamSelection(kAllStreams, FALSE), "deselect_streams");
    check(reader->SetStreamSelection(kFirstVideoStream, TRUE), "select_video");
    ComPtr<IMFMediaType> native;
    check(reader->GetNativeMediaType(kFirstVideoStream, kCurrentType, &native), "native_type");
    const auto input = layout(native.Get(), limits, false);
    ComPtr<IMFMediaType> requested;
    check(MFCreateMediaType(&requested), "requested_type");
    check(requested->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video), "requested_video");
    check(requested->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_RGB32), "requested_rgb32");
    check(MFSetAttributeSize(requested.Get(), MF_MT_FRAME_SIZE, input.width, input.height), "requested_size");
    check(reader->SetCurrentMediaType(kFirstVideoStream, nullptr, requested.Get()), "set_rgb32");
    auto frame = checked_output(reader.Get(), limits, input);
    for (uint64_t reads = 1; reads <= limits.reads; ++reads) {
        DWORD actual = 0, flags = 0;
        LONGLONG timestamp = 0;
        ComPtr<IMFSample> sample;
        check(reader->ReadSample(kFirstVideoStream, 0, &actual, &flags, &timestamp, &sample), "decode");
        validate_read(reader.Get(), flags, limits, input, frame);
        if (!sample) continue;
        DWORD total = 0;
        check(sample->GetTotalLength(&total), "sample_length");
        if (total < frame.bytes || total > limits.bytes) throw Failure{"sample_bound", E_FAIL};
        ComPtr<IMFMediaBuffer> buffer;
        // Check total length BEFORE this API can allocate a contiguous copy.
        check(sample->ConvertToContiguousBuffer(&buffer), "contiguous_buffer");
        LockedBuffer locked(buffer.Get());
        if (!locked.data || locked.length != total || locked.capacity < total || locked.capacity > limits.bytes)
            throw Failure{"invalid_buffer", E_FAIL};
        // Read every visible RGB byte (ignore RGB32's undefined fourth byte and
        // row padding). Black is valid: the pinned sample begins with a fade-in.
        uint64_t checksum = 14695981039346656037ull;
        for (UINT32 y = 0; y < frame.height; ++y) {
            const BYTE* row = locked.data + uint64_t(y) * frame.stride;
            for (UINT32 x = 0; x < frame.width; ++x)
                for (unsigned c = 0; c < 3; ++c)
                    checksum = (checksum ^ row[uint64_t(x) * 4 + c]) * 1099511628211ull;
        }
        return {frame, total, reads, checksum, timestamp};
    }
    throw Failure{"read_limit", E_FAIL};
}

int wmain(int argc, wchar_t** argv) {
    const auto start = std::chrono::steady_clock::now();
    const auto elapsed = [&]() {
        return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - start).count();
    };
    try {
        if (argc != 5) throw Failure{"arguments", E_INVALIDARG};
        Limits limits{positive(argv[2]), positive(argv[3]), positive(argv[4])};
        if (limits.dimension > std::numeric_limits<UINT32>::max() ||
            limits.bytes > std::numeric_limits<DWORD>::max() ||
            limits.reads == std::numeric_limits<uint64_t>::max())
            throw Failure{"arguments", E_INVALIDARG};
        const auto result = decode(argv[1], limits);
        std::printf("{\"ok\":true,\"backend\":\"mf-source-reader\",\"codec\":\"h264\",\"pixel_format\":\"rgb32\","
                    "\"width\":%u,\"height\":%u,\"stride\":%llu,\"bytes\":%lu,\"reads\":%llu,"
                    "\"rgb_fnv1a64\":\"%016llx\",\"timestamp_100ns\":%lld,\"elapsed_ms\":%.3f}\n",
                    result.frame.width, result.frame.height, result.frame.stride, result.bytes,
                    result.reads, result.checksum, result.timestamp, elapsed());
        return 0;
    } catch (const Failure& failure) {
        std::printf("{\"ok\":false,\"stage\":\"%s\",\"hresult\":\"%08lx\",\"elapsed_ms\":%.3f",
                    failure.stage, static_cast<unsigned long>(failure.hr), elapsed());
        if (failure.flags) std::printf(",\"flags\":%lu", failure.flags);
        std::printf("}\n");
    } catch (const std::exception&) {
        std::printf("{\"ok\":false,\"stage\":\"arguments\",\"hresult\":\"80070057\",\"elapsed_ms\":%.3f}\n", elapsed());
    }
    return 1;
}
