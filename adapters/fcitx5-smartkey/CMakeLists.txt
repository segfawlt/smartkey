cmake_minimum_required(VERSION 3.20)
project(fcitx5-smartkey VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_CXX_VISIBILITY_PRESET hidden)
set(CMAKE_VISIBILITY_INLINES_HIDDEN ON)

# Ensure user-local installs (`-DCMAKE_INSTALL_PREFIX=$HOME/.local`) place `libsmartkey.so` in
# `~/.local/lib/fcitx5` (matching Fcitx5's StandardPaths lookup on Arch/Debian/Fedora) rather
# than `lib64/fcitx5`.
if(NOT DEFINED CMAKE_INSTALL_LIBDIR)
    set(CMAKE_INSTALL_LIBDIR "lib" CACHE PATH "Output directory for libraries")
endif()

find_package(Fcitx5Core 5.0.0 REQUIRED)
find_package(Fcitx5Utils 5.0.0 REQUIRED)

get_filename_component(SMARTKEY_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
set(SMARTKEY_FFI_LIB "${SMARTKEY_ROOT}/target/release/libsmartkey_ffi.a")

find_program(CARGO_EXECUTABLE cargo HINTS "$ENV{HOME}/.cargo/bin")
if(NOT CARGO_EXECUTABLE)
    message(FATAL_ERROR "cargo not found; install Rust toolchain (rustup) to build smartkey-ffi")
endif()

add_custom_target(smartkey_ffi_build ALL
    COMMAND "${CARGO_EXECUTABLE}" build --release --manifest-path "${SMARTKEY_ROOT}/Cargo.toml" -p smartkey-ffi
    BYPRODUCTS "${SMARTKEY_FFI_LIB}"
    WORKING_DIRECTORY "${SMARTKEY_ROOT}"
    COMMENT "Building Rust static library libsmartkey_ffi.a"
)

add_library(smartkey MODULE
    src/smartkey-addon.cpp
)
add_dependencies(smartkey smartkey_ffi_build)

set_target_properties(smartkey PROPERTIES
    PREFIX "lib"
    OUTPUT_NAME "smartkey"
)

target_include_directories(smartkey PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/src"
    "${SMARTKEY_ROOT}/crates/smartkey-ffi/include"
)

target_compile_options(smartkey PRIVATE
    -Wall
    -Wextra
    -Wpedantic
)

target_link_libraries(smartkey PRIVATE
    Fcitx5::Core
    Fcitx5::Utils
    "${SMARTKEY_FFI_LIB}"
    pthread
    dl
    m
)

# Hide all internal Rust/std symbols inside `libsmartkey.so` so it cannot collide with any
# other Rust-backed Fcitx5 addon (such as `fcitx5-lotus`) loaded in the same process.
target_link_options(smartkey PRIVATE "-Wl,--exclude-libs,ALL")

# Fcitx5 searches `~/.local/share/fcitx5/addon/*.conf` (`PkgData`) automatically in user space,
# whereas `StandardPathsType::Addon` only searches `/usr/lib/fcitx5` unless `Library=` specifies
# the user-local path. Use a bare `libsmartkey` for `/usr` system packages and the full addon
# library path for user-local (`~/.local`) installs so zero `sudo` or env vars are required.
if(FCITX_INSTALL_USE_FCITX_SYS_PATHS)
    set(SMARTKEY_LIBRARY_SPEC "libsmartkey")
else()
    set(SMARTKEY_LIBRARY_SPEC "${FCITX_INSTALL_ADDONDIR}/libsmartkey")
endif()

configure_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/smartkey.conf.in"
    "${CMAKE_CURRENT_BINARY_DIR}/smartkey.conf"
    @ONLY
)

install(TARGETS smartkey DESTINATION "${FCITX_INSTALL_ADDONDIR}")
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/smartkey.conf" DESTINATION "${FCITX_INSTALL_PKGDATADIR}/addon")

enable_testing()
add_executable(addon_load_smoke tests/addon_load_smoke.cpp)
target_link_libraries(addon_load_smoke PRIVATE Fcitx5::Core Fcitx5::Utils)
add_dependencies(addon_load_smoke smartkey)
add_test(
    NAME addon_load_smoke
    COMMAND addon_load_smoke "$<TARGET_FILE:smartkey>"
)

