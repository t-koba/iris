# Prototype-only custom dynamic triplet for the isolated Windows preview prototype.
# Test-only; never a release payload. Functionally the x64-windows dynamic
# linkage (dynamic CRT + dynamic libraries) under an isolated triplet name so
# the prototype install never collides with release/default installs.
set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE dynamic)
# Fail closed on any static linkage request; the prototype is dynamic-only.
if(VCPKG_CRT_LINKAGE STREQUAL "static")
  message(FATAL_ERROR "prototype triplet must stay dynamic (CRT)")
endif()
if(VCPKG_LIBRARY_LINKAGE STREQUAL "static")
  message(FATAL_ERROR "prototype triplet must stay dynamic (library)")
endif()
