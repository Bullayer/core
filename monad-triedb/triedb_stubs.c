// Stub implementations for triedb C symbols.
// Used when the real C++ triedb library is not available (TRIEDB_TARGET not set).
// All functions return "not found" / "empty" / "failure" codes safely.

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <stdlib.h>

typedef struct triedb triedb;
typedef uint8_t const *bytes;

enum triedb_async_traverse_callback {
    triedb_async_traverse_callback_value,
    triedb_async_traverse_callback_finished_normally,
    triedb_async_traverse_callback_finished_early
};

typedef void (*callback_func)(
    enum triedb_async_traverse_callback kind, void *context, bytes path,
    size_t path_len, bytes value, size_t value_len);

typedef struct validator_data {
    uint8_t secp_pubkey[33];
    uint8_t bls_pubkey[48];
    uint8_t stake[32];
} validator_data;

typedef struct validator_set {
    struct validator_data *validators;
    uint64_t length;
} validator_set;

int triedb_open(char const *dbdirpath, triedb **db, uint64_t node_lru_max_mem) {
    (void)dbdirpath; (void)db; (void)node_lru_max_mem;
    return -1;
}

int triedb_close(triedb *db) {
    (void)db;
    return 0;
}

int triedb_read(triedb *db, bytes key, uint8_t key_len_nibbles, bytes *value, uint64_t block_id) {
    (void)db; (void)key; (void)key_len_nibbles; (void)value; (void)block_id;
    return -1;
}

void triedb_async_read(triedb *db, bytes key, uint8_t key_len_nibbles, uint64_t block_id,
    void (*completed)(bytes value, int length, void *user), void *user) {
    (void)db; (void)key; (void)key_len_nibbles; (void)block_id;
    if (completed) completed(NULL, -1, user);
}

bool triedb_traverse(triedb *db, bytes key, uint8_t key_len_nibbles, uint64_t block_id,
    void *context, callback_func callback) {
    (void)db; (void)key; (void)key_len_nibbles; (void)block_id;
    if (callback) callback(triedb_async_traverse_callback_finished_normally, context, NULL, 0, NULL, 0);
    return false;
}

void triedb_async_traverse(triedb *db, bytes key, uint8_t key_len_nibbles, uint64_t block_id,
    void *context, callback_func callback) {
    (void)db; (void)key; (void)key_len_nibbles; (void)block_id;
    if (callback) callback(triedb_async_traverse_callback_finished_normally, context, NULL, 0, NULL, 0);
}

void triedb_async_ranged_get(triedb *db, bytes prefix_key, uint8_t prefix_len_nibbles,
    bytes min_key, uint8_t min_len_nibbles, bytes max_key, uint8_t max_len_nibbles,
    uint64_t block_id, void *context, callback_func callback) {
    (void)db; (void)prefix_key; (void)prefix_len_nibbles;
    (void)min_key; (void)min_len_nibbles; (void)max_key; (void)max_len_nibbles; (void)block_id;
    if (callback) callback(triedb_async_traverse_callback_finished_normally, context, NULL, 0, NULL, 0);
}

size_t triedb_poll(triedb *db, bool blocking, size_t count) {
    (void)db; (void)blocking; (void)count;
    return 0;
}

int triedb_finalize(bytes value) {
    (void)value;
    return 0;
}

uint64_t triedb_latest_proposed_block(triedb *db) {
    (void)db;
    return UINT64_MAX;
}

bytes triedb_latest_proposed_block_id(triedb *db) {
    (void)db;
    return NULL;
}

uint64_t triedb_latest_voted_block(triedb *db) {
    (void)db;
    return UINT64_MAX;
}

bytes triedb_latest_voted_block_id(triedb *db) {
    (void)db;
    return NULL;
}

uint64_t triedb_latest_finalized_block(triedb *db) {
    (void)db;
    return UINT64_MAX;
}

uint64_t triedb_latest_verified_block(triedb *db) {
    (void)db;
    return UINT64_MAX;
}

uint64_t triedb_earliest_finalized_block(triedb *db) {
    (void)db;
    return UINT64_MAX;
}

void free_valset(validator_set *vs) {
    (void)vs;
}

validator_set *read_valset(triedb *db, size_t block_num, uint64_t requested_epoch) {
    (void)db; (void)block_num; (void)requested_epoch;
    return NULL;
}
