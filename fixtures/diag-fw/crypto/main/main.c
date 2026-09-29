/* Crypto diagnostic for the NOTE Emulator (fixtures/diag-fw/crypto).
 * Runs SHA-256/384/512 and AES-GCM through PSA, i.e. through the same ESP32-S3 accelerator
 * drivers TLS uses, and prints every result as "DIAG <what> <hex>". The host test compares them
 * with reference implementations. Deterministic inputs; no randomness. */
#include <stdio.h>
#include <string.h>
#include "psa/crypto.h"

static uint8_t buf[8192];
static uint8_t out[8192 + 16];

static void hex(const char *what, unsigned arg, const uint8_t *b, size_t n)
{
    printf("DIAG %s %u ", what, arg);
    for (size_t i = 0; i < n; i++) printf("%02x", b[i]);
    printf("\n");
}

static void hash(const char *name, psa_algorithm_t alg, size_t digest)
{
    static const size_t lens[] = {0, 3, 55, 56, 63, 64, 65, 111, 112, 127, 128, 129, 255, 256, 1000, 4096, 8191};
    for (size_t i = 0; i < sizeof lens / sizeof lens[0]; i++) {
        uint8_t d[64];
        size_t len = 0;
        psa_status_t st = psa_hash_compute(alg, buf, lens[i], d, digest, &len);
        if (st != PSA_SUCCESS) { printf("DIAG %s %u error %d\n", name, (unsigned)lens[i], (int)st); continue; }
        hex(name, lens[i], d, len);
    }
}

static void gcm(const char *name, const uint8_t *key, size_t key_len, size_t pt_len)
{
    psa_key_attributes_t attr = PSA_KEY_ATTRIBUTES_INIT;
    psa_set_key_usage_flags(&attr, PSA_KEY_USAGE_ENCRYPT);
    psa_set_key_algorithm(&attr, PSA_ALG_GCM);
    psa_set_key_type(&attr, PSA_KEY_TYPE_AES);
    psa_set_key_bits(&attr, key_len * 8);
    psa_key_id_t id;
    if (psa_import_key(&attr, key, key_len, &id) != PSA_SUCCESS) { printf("DIAG %s %u error import\n", name, (unsigned)pt_len); return; }
    static const uint8_t nonce[12] = {0xca, 0xfe, 0xba, 0xbe, 0xfa, 0xce, 0xdb, 0xad, 0xde, 0xca, 0xf8, 0x88};
    static const uint8_t aad[20] = {0xfe, 0xed, 0xfa, 0xce, 0xde, 0xad, 0xbe, 0xef, 0xfe, 0xed, 0xfa, 0xce, 0xde, 0xad, 0xbe, 0xef, 0xab, 0xad, 0xda, 0xd2};
    size_t n = 0;
    psa_set_key_usage_flags(&attr, PSA_KEY_USAGE_ENCRYPT | PSA_KEY_USAGE_DECRYPT);
    psa_destroy_key(id);
    if (psa_import_key(&attr, key, key_len, &id) != PSA_SUCCESS) { printf("DIAG %s %u error import\n", name, (unsigned)pt_len); return; }
    psa_status_t st = psa_aead_encrypt(id, PSA_ALG_GCM, nonce, sizeof nonce, aad, sizeof aad, buf, pt_len, out, sizeof out, &n);
    if (st == PSA_SUCCESS) {
        /* Decrypt what we just sealed (TLS decrypts every server record): plaintext and tag check. */
        static uint8_t back[8192];
        size_t m = 0;
        psa_status_t dt = psa_aead_decrypt(id, PSA_ALG_GCM, nonce, sizeof nonce, aad, sizeof aad, out, n, back, sizeof back, &m);
        printf("DIAG %s-rt %u %s\n", name, (unsigned)pt_len, dt == PSA_SUCCESS && m == pt_len && !memcmp(back, buf, pt_len) ? "ok" : "bad");
        /* A flipped tag bit must be refused. */
        out[n - 1] ^= 1;
        dt = psa_aead_decrypt(id, PSA_ALG_GCM, nonce, sizeof nonce, aad, sizeof aad, out, n, back, sizeof back, &m);
        out[n - 1] ^= 1;
        printf("DIAG %s-forged %u %s\n", name, (unsigned)pt_len, dt == PSA_ERROR_INVALID_SIGNATURE ? "ok" : "bad");
    }
    psa_destroy_key(id);
    if (st != PSA_SUCCESS) { printf("DIAG %s %u error %d\n", name, (unsigned)pt_len, (int)st); return; }
    /* Ciphertext can be long: print its first 32 bytes, its last 32 bytes, and the 16-byte tag. */
    size_t ct = n - 16;
    hex(name, pt_len, out, ct < 32 ? ct : 32);
    if (ct > 32) { char tail[40]; snprintf(tail, sizeof tail, "%s-tail", name); hex(tail, pt_len, out + ct - 32, 32); }
    char tag[40]; snprintf(tag, sizeof tag, "%s-tag", name); hex(tag, pt_len, out + ct, 16);
}

/* What TLS does with its transcript hash: two contexts updated in turn with different chunk
 * sizes (the accelerator has one state, so each switch saves and reloads it), and a context
 * cloned mid-stream. Each line reports how many bytes the digest covers. */
static void incremental(const char *name, psa_algorithm_t alg, size_t digest)
{
    static const size_t lens[] = {300, 1000, 4096};
    for (size_t i = 0; i < sizeof lens / sizeof lens[0]; i++) {
        size_t len = lens[i], pa = 0, pb = 0, clone_at = 0, n;
        psa_hash_operation_t a = PSA_HASH_OPERATION_INIT, b = PSA_HASH_OPERATION_INIT, c = PSA_HASH_OPERATION_INIT;
        if (psa_hash_setup(&a, alg) != PSA_SUCCESS || psa_hash_setup(&b, alg) != PSA_SUCCESS) { printf("DIAG %s-inc %u error setup\n", name, (unsigned)len); return; }
        while (pa < len || pb < len) {
            if (pa < len) {
                n = len - pa < 37 ? len - pa : 37;
                psa_hash_update(&a, buf + pa, n);
                pa += n;
                if (!clone_at && pa >= len / 2) { psa_hash_clone(&a, &c); clone_at = pa; }
            }
            if (pb < len) {
                n = len - pb < 101 ? len - pb : 101;
                psa_hash_update(&b, buf + pb, n);
                pb += n;
            }
        }
        uint8_t d[64];
        char label[40];
        snprintf(label, sizeof label, "%s-inc", name);
        if (psa_hash_finish(&a, d, digest, &n) == PSA_SUCCESS) hex(label, len, d, n);
        snprintf(label, sizeof label, "%s-inc2", name);
        if (psa_hash_finish(&b, d, digest, &n) == PSA_SUCCESS) hex(label, len, d, n);
        snprintf(label, sizeof label, "%s-clone", name);
        if (psa_hash_finish(&c, d, digest, &n) == PSA_SUCCESS) hex(label, clone_at, d, n);
    }
}

/* HMAC (the HKDF inside TLS 1.3) with a 20-byte key of 0x0b. */
static void hmac(const char *name, psa_algorithm_t hash_alg)
{
    static const uint8_t key[20] = {0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b,
                                    0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b, 0x0b};
    psa_key_attributes_t attr = PSA_KEY_ATTRIBUTES_INIT;
    psa_set_key_usage_flags(&attr, PSA_KEY_USAGE_SIGN_MESSAGE);
    psa_set_key_algorithm(&attr, PSA_ALG_HMAC(hash_alg));
    psa_set_key_type(&attr, PSA_KEY_TYPE_HMAC);
    psa_key_id_t id;
    if (psa_import_key(&attr, key, sizeof key, &id) != PSA_SUCCESS) { printf("DIAG %s 0 error import\n", name); return; }
    static const size_t lens[] = {0, 50, 200, 1000};
    for (size_t i = 0; i < sizeof lens / sizeof lens[0]; i++) {
        uint8_t mac[64];
        size_t n = 0;
        if (psa_mac_compute(id, PSA_ALG_HMAC(hash_alg), buf, lens[i], mac, sizeof mac, &n) == PSA_SUCCESS) hex(name, lens[i], mac, n);
        else printf("DIAG %s %u error mac\n", name, (unsigned)lens[i]);
    }
    psa_destroy_key(id);
}

void app_main(void)
{
    for (size_t i = 0; i < sizeof buf; i++) buf[i] = (uint8_t)(i * 7 + 3);
    if (psa_crypto_init() != PSA_SUCCESS) { printf("DIAG error psa_crypto_init\n"); return; }
    hash("sha256", PSA_ALG_SHA_256, 32);
    hash("sha384", PSA_ALG_SHA_384, 48);
    hash("sha512", PSA_ALG_SHA_512, 64);
    static const uint8_t k256[32] = {0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30, 0x83, 0x08,
                                     0xfe, 0xff, 0xe9, 0x92, 0x86, 0x65, 0x73, 0x1c, 0x6d, 0x6a, 0x8f, 0x94, 0x67, 0x30, 0x83, 0x08};
    static const size_t gcm_lens[] = {0, 16, 60, 64, 255, 256, 1024, 4096};
    for (size_t i = 0; i < sizeof gcm_lens / sizeof gcm_lens[0]; i++) {
        gcm("gcm128", k256, 16, gcm_lens[i]);
        gcm("gcm256", k256, 32, gcm_lens[i]);
    }
    incremental("sha256", PSA_ALG_SHA_256, 32);
    incremental("sha384", PSA_ALG_SHA_384, 48);
    incremental("sha512", PSA_ALG_SHA_512, 64);
    hmac("hmac256", PSA_ALG_SHA_256);
    hmac("hmac384", PSA_ALG_SHA_384);
    hmac("hmac512", PSA_ALG_SHA_512);
    printf("DIAG done\n");
}
