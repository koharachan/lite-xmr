if (CMAKE_SIZEOF_VOID_P EQUAL 8)
    set(XMRIG_64_BIT ON)
    add_definitions(-DXMRIG_64_BIT)
else()
    set(XMRIG_64_BIT OFF)
    message(FATAL_ERROR "32-bit architecture is not supported. Only x86_64 is supported.")
endif()

if (NOT CMAKE_SYSTEM_PROCESSOR)
    message(WARNING "CMAKE_SYSTEM_PROCESSOR not defined")
endif()

include(CheckCXXCompilerFlag)

if (CMAKE_CXX_COMPILER_ID MATCHES MSVC)
    set(VAES_SUPPORTED ON)
else()
    CHECK_CXX_COMPILER_FLAG("-mavx2 -mvaes" VAES_SUPPORTED)
endif()

if (NOT VAES_SUPPORTED)
    set(WITH_VAES OFF)
endif()

# Only x86_64 is supported
if (XMRIG_64_BIT AND CMAKE_SYSTEM_PROCESSOR MATCHES "^(x86_64|AMD64)$")
    add_definitions(-DRAPIDJSON_SSE2)
else()
    message(FATAL_ERROR "Unsupported architecture. Only x86_64 is supported.")
endif()

add_definitions(-DRAPIDJSON_WRITE_DEFAULT_FLAGS=6) # rapidjson::kWriteNanAndInfFlag | rapidjson::kWriteNanAndInfNullFlag

# ARM and RISC-V are not supported
set(ARM_TARGET 0)
set(XMRIG_ARM OFF)
set(XMRIG_RISCV OFF)

if (WITH_SSE4_1)
    add_definitions(-DXMRIG_FEATURE_SSE4_1)
endif()

if (WITH_AVX2)
    add_definitions(-DXMRIG_FEATURE_AVX2)
endif()
