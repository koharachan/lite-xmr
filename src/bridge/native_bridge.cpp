#include <cstddef>
#include <cstdint>
#include <cstring>

#include "3rdparty/rapidjson/document.h"
#include "3rdparty/rapidjson/stringbuffer.h"
#include "3rdparty/rapidjson/writer.h"
#include "crypto/randomx/configuration.h"
#include "crypto/randomx/randomx.h"

extern "C" {

std::size_t lx_randomx_hash_size() {
    return RANDOMX_HASH_SIZE;
}

std::uint64_t lx_randomx_dataset_max_size() {
    return RANDOMX_DATASET_MAX_SIZE;
}

int lx_randomx_apply_config(const char* algo) {
    if (algo == nullptr || std::strcmp(algo, "rx/0") == 0 || std::strcmp(algo, "randomx") == 0 ||
        std::strcmp(algo, "randomx/0") == 0) {
        return 1;
    }
    return 0;
}

int lx_rapidjson_minify(const char* input, char* output, std::size_t output_capacity, std::size_t* output_len) {
    if (input == nullptr || output == nullptr || output_len == nullptr || output_capacity == 0) {
        return 0;
    }

    rapidjson::Document doc;
    doc.Parse(input);
    if (doc.HasParseError()) {
        return 0;
    }

    rapidjson::StringBuffer buffer;
    rapidjson::Writer<rapidjson::StringBuffer> writer(buffer);
    if (!doc.Accept(writer)) {
        return 0;
    }

    const std::size_t len = buffer.GetSize();
    if (len + 1 > output_capacity) {
        return 0;
    }

    std::memcpy(output, buffer.GetString(), len);
    output[len] = '\0';
    *output_len = len;
    return 1;
}

}  // extern "C"
