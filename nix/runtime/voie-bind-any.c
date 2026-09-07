#define _GNU_SOURCE
#include <dlfcn.h>
#include <netinet/in.h>
#include <string.h>
#include <sys/socket.h>

/* Application guests often bind 127.0.0.1. In-guest healthz then passes while
   the Fabric gateway cannot reach the Pod IP. Rewrite loopback binds to all
   interfaces so ClusterIP traffic is admitted. */

int bind(int sockfd, const struct sockaddr *addr, socklen_t addrlen) {
    static int (*real_bind)(int, const struct sockaddr *, socklen_t) = NULL;
    if (real_bind == NULL) {
        real_bind = (int (*)(int, const struct sockaddr *, socklen_t))dlsym(
            RTLD_NEXT, "bind");
    }
    if (addr != NULL && addr->sa_family == AF_INET &&
        addrlen >= (socklen_t)sizeof(struct sockaddr_in)) {
        struct sockaddr_in copy;
        memcpy(&copy, addr, sizeof(copy));
        if (copy.sin_addr.s_addr == htonl(INADDR_LOOPBACK)) {
            copy.sin_addr.s_addr = htonl(INADDR_ANY);
            return real_bind(sockfd, (const struct sockaddr *)&copy, sizeof(copy));
        }
    }
    if (addr != NULL && addr->sa_family == AF_INET6 &&
        addrlen >= (socklen_t)sizeof(struct sockaddr_in6)) {
        struct sockaddr_in6 copy;
        static const unsigned char loopback[16] = {
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1};
        memcpy(&copy, addr, sizeof(copy));
        if (memcmp(copy.sin6_addr.s6_addr, loopback, 16) == 0) {
            memset(&copy.sin6_addr, 0, sizeof(copy.sin6_addr));
            return real_bind(sockfd, (const struct sockaddr *)&copy, sizeof(copy));
        }
    }
    return real_bind(sockfd, addr, addrlen);
}
