# Production validation dynamic triplet. Isolated name so validation installs
# never collide with release/default installs. Dynamic CRT + dynamic libraries.
# Non-publishing until exact Windows-runner evidence passes.
set(VCPKG_TARGET_ARCHITECTURE x64)
set(VCPKG_CRT_LINKAGE dynamic)
set(VCPKG_LIBRARY_LINKAGE dynamic)
if(VCPKG_CRT_LINKAGE STREQUAL "static")
  message(FATAL_ERROR "validation triplet must stay dynamic (CRT)")
endif()
if(VCPKG_LIBRARY_LINKAGE STREQUAL "static")
  message(FATAL_ERROR "validation triplet must stay dynamic (library)")
endif()
