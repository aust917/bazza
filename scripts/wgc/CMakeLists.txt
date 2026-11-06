cmake_minimum_required(VERSION 3.22)
project(bazza_wgc LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
    message(FATAL_ERROR "Bazza WGC requires the MSVC toolchain and Windows SDK")
endif()

add_executable(bazza-wgc encode.cpp wgc_sidecar.cpp)
set_property(TARGET bazza-wgc PROPERTY MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
target_compile_features(bazza-wgc PRIVATE cxx_std_20)
target_compile_definitions(bazza-wgc PRIVATE BAZZA_WGC_INTEGRATED)
target_compile_options(bazza-wgc PRIVATE /W4 /EHsc)
target_link_libraries(bazza-wgc PRIVATE d3d11 user32 windowsapp)
