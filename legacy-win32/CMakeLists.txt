cmake_minimum_required(VERSION 3.15)
project(SecurityMonitoringLegacy LANGUAGES CXX)

add_executable(SecurityMonitoringLegacy WIN32 src/main.cpp)
add_executable(SecurityMonitoring WIN32 src/launcher.cpp)
target_compile_features(SecurityMonitoringLegacy PRIVATE cxx_std_14)
target_compile_features(SecurityMonitoring PRIVATE cxx_std_14)
target_compile_definitions(SecurityMonitoringLegacy PRIVATE
  UNICODE _UNICODE WIN32_LEAN_AND_MEAN _WIN32_WINNT=0x0501
  NTDDI_VERSION=0x05010300)
target_compile_definitions(SecurityMonitoring PRIVATE UNICODE _UNICODE WIN32_LEAN_AND_MEAN _WIN32_WINNT=0x0501)
target_link_libraries(SecurityMonitoringLegacy PRIVATE user32 gdi32 gdiplus advapi32 shell32 psapi ole32)

if (MSVC)
  # XP SP3 is the minimum supported system. The XP toolset is required for
  # the CRT and linker; current MSVC toolsets no longer ship these libraries.
  target_link_options(SecurityMonitoringLegacy PRIVATE /SUBSYSTEM:WINDOWS,5.01)
  target_link_options(SecurityMonitoring PRIVATE /SUBSYSTEM:WINDOWS,5.01)
  target_compile_options(SecurityMonitoringLegacy PRIVATE /W4 /EHsc /MT /D_CRT_SECURE_NO_WARNINGS)
  target_compile_options(SecurityMonitoring PRIVATE /W4 /EHsc /MT)
endif()
