#ifndef CENSORSCOPE_CONST_H
#define CENSORSCOPE_CONST_H

/* Must match EXEC_FILENAME_MAX in the Rust decoder. */
#define EXEC_FILENAME_ABI_MAX_BYTES 512
#define EXEC_ARG_MAX 16
#define EXEC_ARG_BYTES_ABI_MAX 1024
#define EXEC_ARG_SLOT_ABI_MAX_BYTES \
    (EXEC_ARG_BYTES_ABI_MAX / EXEC_ARG_MAX)

#define TLS_PAYLOAD_ABI_MAX_BYTES 4096
#define TLS_MAX_CHUNKS 64
#define FILE_PATH_ABI_MAX_BYTES 512

/* Session identity is captured in-kernel from CENSORSCOPE_SESSION_ID. The legacy
 * DSH_SESSION_ID name remains a fallback for older processes. */
#define SESSION_LEN 128
#define CALL_ID_LEN 128
#define ENV_BUF_LEN 512
#define MAX_ENV_VARS 96


enum censorscope_exec_flag {
    EXEC_FILENAME_FLAG_TRUNCATED = 1,
    EXEC_ARG_FLAG_PARTIAL = 1,
    EXEC_ARG_FLAG_TRUNCATED = 2,
    EXEC_ARG_FLAG_READ_FAILURE = 4,
    EXEC_ARG_FLAG_LOSS = 8,
};

enum censorscope_tls_value {
    TLS_DIRECTION_INBOUND = 1,
    TLS_DIRECTION_OUTBOUND = 2,
    TLS_FLAG_READ_FAILURE = 1,
    TLS_FLAG_TRUNCATED = 2,
    TLS_CHUNK_FLAG_START = 4,
    TLS_CHUNK_FLAG_END = 8,
};

enum censorscope_file_path_flag {
    FILE_PATH_FLAG_TRUNCATED = 1,
    FILE_PATH_FLAG_CAPTURE_GAP = 2,
};

#endif
