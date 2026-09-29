#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <dispatch/dispatch.h>
#include <xpc/xpc.h>
#include <vmnet/vmnet.h>

/* Start one shared-mode interface and always stop it before returning.
   This slice does not relay guest frames, so a live interface must not be
   left behind. The strings are the interface parameters vmnet reports
   (gateway range and MAC). They are not a guest address. */

static void copy_str(char *dst, size_t dst_len, const char *src) {
    if (dst == NULL || dst_len == 0) {
        return;
    }
    if (src == NULL) {
        dst[0] = 0;
        return;
    }
    strlcpy(dst, src, dst_len);
}

uint32_t note_vmnet_probe_shared(char *mac, size_t mac_len,
                                 char *start_addr, size_t start_len,
                                 char *end_addr, size_t end_len,
                                 char *mask, size_t mask_len,
                                 uint64_t *mtu_out) {
    copy_str(mac, mac_len, "");
    copy_str(start_addr, start_len, "");
    copy_str(end_addr, end_len, "");
    copy_str(mask, mask_len, "");
    if (mtu_out != NULL) {
        *mtu_out = 0;
    }

    xpc_object_t desc = xpc_dictionary_create(NULL, NULL, 0);
    xpc_dictionary_set_uint64(desc, vmnet_operation_mode_key, VMNET_SHARED_MODE);
    dispatch_queue_t queue = dispatch_queue_create("note.vmnet.shared", DISPATCH_QUEUE_SERIAL);
    dispatch_semaphore_t sem = dispatch_semaphore_create(0);

    __block vmnet_return_t status = VMNET_FAILURE;
    __block int finished = 0;
    __block char *mac_b = NULL;
    __block char *start_b = NULL;
    __block char *end_b = NULL;
    __block char *mask_b = NULL;
    __block uint64_t mtu_b = 0;

    interface_ref iface = vmnet_start_interface(desc, queue, ^(vmnet_return_t st, xpc_object_t param) {
        status = st;
        if (st == VMNET_SUCCESS && param != NULL) {
            const char *value;
            value = xpc_dictionary_get_string(param, vmnet_mac_address_key);
            if (value != NULL) mac_b = strdup(value);
            value = xpc_dictionary_get_string(param, vmnet_start_address_key);
            if (value != NULL) start_b = strdup(value);
            value = xpc_dictionary_get_string(param, vmnet_end_address_key);
            if (value != NULL) end_b = strdup(value);
            value = xpc_dictionary_get_string(param, vmnet_subnet_mask_key);
            if (value != NULL) mask_b = strdup(value);
            mtu_b = xpc_dictionary_get_uint64(param, vmnet_mtu_key);
        }
        finished = 1;
        dispatch_semaphore_signal(sem);
    });

    if (dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 8LL * NSEC_PER_SEC)) != 0 || !finished) {
        status = VMNET_SETUP_INCOMPLETE;
    }

    if (iface != NULL) {
        dispatch_semaphore_t stop_sem = dispatch_semaphore_create(0);
        vmnet_return_t scheduled = vmnet_stop_interface(iface, queue, ^(vmnet_return_t stop_status) {
            (void)stop_status;
            dispatch_semaphore_signal(stop_sem);
        });
        if (scheduled == VMNET_SUCCESS) {
            dispatch_semaphore_wait(stop_sem, dispatch_time(DISPATCH_TIME_NOW, 3LL * NSEC_PER_SEC));
        }
        dispatch_release(stop_sem);
    }

    copy_str(mac, mac_len, mac_b);
    copy_str(start_addr, start_len, start_b);
    copy_str(end_addr, end_len, end_b);
    copy_str(mask, mask_len, mask_b);
    if (mtu_out != NULL) {
        *mtu_out = mtu_b;
    }
    free(mac_b);
    free(start_b);
    free(end_b);
    free(mask_b);
    dispatch_release(sem);
    xpc_release(desc);
    return (uint32_t)status;
}
