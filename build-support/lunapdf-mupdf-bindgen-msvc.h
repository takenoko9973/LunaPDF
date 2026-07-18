#ifndef LUNAPDF_MUPDF_BINDGEN_MSVC_H
#define LUNAPDF_MUPDF_BINDGEN_MSVC_H

#if defined(_MSC_VER) && __has_include("mupdf/fitz.h")
// MSVCのmax_align_tはプリミティブ型なので、mupdf-sysのopaque_type指定では
// Rust定義が生成されない。システム定義を退避し、同じサイズとアラインメントの
// レコードとしてbindgenへ提示する。
#define max_align_t mupdf_system_max_align_t
#include <stddef.h>
#undef max_align_t
typedef struct max_align_t {
    double alignment;
} max_align_t;
#endif

#endif
