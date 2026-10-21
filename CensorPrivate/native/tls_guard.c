#define _GNU_SOURCE
#include <openssl/ssl.h>
#include <openssl/err.h>
#include <arpa/inet.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/random.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

/* A cooperating dynamically linked OpenSSL client is required. The local
 * broker owns protocol admission and full-message protection. No raw fallback
 * exists. Only the broker endpoint uses this library's plain HTTP socket. */
#define MAX_PAYLOAD (8u * 1024u * 1024u)
#define MAX_MEMORY (128u * 1024u * 1024u)
#define MAX_STATES 256u
#define READ_STAGE 16384u
#define MAX_HEADER 8192u
#define RPC_TIMEOUT_MS 2000
#define REQ_HEADER 20u
#define RESP_HEADER 12u

enum { ACT_ALLOW=1, ACT_REPLACE=2, ACT_BLOCK=3, ACT_DEFER=4 };
struct backend {
    int (*write)(SSL *, const void *, int);
    int (*write_ex)(SSL *, const void *, size_t, size_t *);
    int (*read)(SSL *, void *, int);
    int (*read_ex)(SSL *, void *, size_t, size_t *);
    int (*get_error)(const SSL *, int);
    int (*get_fd)(const SSL *);
    int (*verify_mode)(const SSL *);
    long (*verify_result)(const SSL *);
    int (*handshake_finished)(const SSL *);
    int (*connect)(SSL *);
    int (*handshake)(SSL *);
    long (*ctrl)(SSL *, int, long, void *);
    SSL *(*new_ssl)(SSL_CTX *);
    int (*up_ref)(SSL *);
    void (*free_ssl)(SSL *);
    int (*pending)(const SSL *);
    int (*has_pending)(const SSL *);
};
static struct backend api;
static pthread_once_t backend_once=PTHREAD_ONCE_INIT;
static struct sockaddr_in broker_address, peer_address;
static char broker_host[64], access_token[513];
static bool configured=false;
static pthread_mutex_t table_lock=PTHREAD_MUTEX_INITIALIZER;
static _Atomic size_t memory_used=0;
static uint64_t stream_seed, stream_counter;
static bool seed_valid=false;
static _Thread_local unsigned rpc_depth;

struct pending_write {
    unsigned char *original, *allocation, *approved;
    const void *caller_pointer;
    size_t original_length, approved_length, offset, reservation;
    bool moving;
    int kind;
};
struct state {
    SSL *ssl;
    uint64_t stream;
    pid_t owner;
    pthread_mutex_t lock;
    unsigned references, tls_references;
    bool closing, poisoned, peer_verified, awaiting_response, binding_ambiguous;
    int fd, bound_fd, read_wait_kind;
    size_t out_held, in_held;
    struct pending_write write;
    unsigned char stage[READ_STAGE];
    unsigned char *safe_allocation, *safe;
    size_t safe_length, safe_offset, safe_reservation;
    struct state *next;
};
static struct state *states;
static size_t state_count;
static SSL *inherited_connections[MAX_STATES];
static unsigned inherited_references[MAX_STATES];
static size_t inherited_count;
struct io_context {
    SSL *ssl;
    struct state *item;
    struct io_context *parent;
    pid_t owner;
    int observed_fd;
    bool handshake, ambiguous;
};
static _Thread_local struct io_context *active_io;
static pthread_once_t socket_once=PTHREAD_ONCE_INIT;
static ssize_t (*real_send)(int,const void *,size_t,int);
static ssize_t (*real_recv)(int,void *,size_t,int);

static void fatal(const char *code) {
    static const char prefix[]="censorprivacy_tls_guard: ";
    (void)!write(STDERR_FILENO, prefix, sizeof(prefix)-1);
    (void)!write(STDERR_FILENO, code, strlen(code));
    (void)!write(STDERR_FILENO, "\n", 1);
    _exit(126);
}
static int fail(int code) {
#if OPENSSL_VERSION_NUMBER >= 0x30000000L
    ERR_raise(ERR_LIB_SSL, ERR_R_INTERNAL_ERROR);
#else
    ERR_put_error(ERR_LIB_SSL, 0, ERR_R_INTERNAL_ERROR, __FILE__, __LINE__);
#endif
    errno=code;
    return -1;
}
static bool peer_failure(const char *code) {
    int preserved=errno;
    static const char prefix[]="censorprivacy_tls_guard: ";
    (void)!write(STDERR_FILENO,prefix,sizeof(prefix)-1);
    (void)!write(STDERR_FILENO,code,strlen(code));
    (void)!write(STDERR_FILENO,"\n",1);
    errno=preserved;
    return false;
}
static void erase(void *pointer, size_t length) {
    volatile unsigned char *bytes=pointer;
    while (length--) *bytes++=0;
}
static bool same_bytes(const void *left, const void *right, size_t length) {
    const unsigned char *a=left, *b=right;
    unsigned difference=0;
    for (size_t i=0;i<length;i++) difference |= a[i]^b[i];
    return difference==0;
}
static bool reserve_memory(size_t count) {
    size_t previous=atomic_load(&memory_used);
    do {
        if (count>MAX_MEMORY || previous>MAX_MEMORY-count) return false;
    } while (!atomic_compare_exchange_weak(&memory_used, &previous, previous+count));
    return true;
}
static void release_memory(size_t count) {
    if (count) atomic_fetch_sub(&memory_used, count);
}
static void seed_streams(void) {
    uint64_t random;
    ssize_t amount;
    do { amount=getrandom(&random, sizeof(random), GRND_NONBLOCK); } while(amount<0 && errno==EINTR);
    seed_valid=amount==(ssize_t)sizeof(random);
    if (seed_valid) stream_seed=random ^ ((uint64_t)(unsigned)getpid()<<32);
    stream_counter=0;
}
static bool parse_endpoint(const char *input, struct sockaddr_in *address, bool local, char *host) {
    if (!input || strlen(input)>80) return false;
    char copy[81]; strcpy(copy,input);
    char *colon=strchr(copy,':');
    if (!colon || strchr(colon+1,':') || colon==copy || !colon[1]) return false;
    *colon++=0;
    for (const char *cursor=colon;*cursor;cursor++) if (*cursor<'0'||*cursor>'9') return false;
    unsigned long port=strtoul(colon,NULL,10);
    if (!port||port>65535) return false;
    memset(address,0,sizeof(*address)); address->sin_family=AF_INET;
    if (inet_pton(AF_INET,copy,&address->sin_addr)!=1) return false;
    uint32_t ip=ntohl(address->sin_addr.s_addr);
    if (local && ip!=0x7f000001u && (ip&0xfffe0000u)!=0xc6120000u) return false;
    address->sin_port=htons((uint16_t)port);
    if (host) snprintf(host,64,"%s:%lu",copy,port);
    return true;
}
static void before_fork(void) { pthread_mutex_lock(&table_lock); }
static void after_fork_parent(void) { pthread_mutex_unlock(&table_lock); }
static void after_fork_child(void) {
    for (struct state *item=states;item;item=item->next) {
        if(inherited_count==MAX_STATES) fatal("fork_state_limit");
        inherited_connections[inherited_count]=item->ssl;
        inherited_references[inherited_count++]=item->tls_references;
    }
    /* Parent-owned mutexes and pending buffers are never reused in the child.
     * Their inherited allocations are deliberately abandoned, not unlocked. */
    states=NULL; state_count=0; atomic_store(&memory_used,0);
    rpc_depth=0;
    active_io=NULL;
    seed_valid=false; stream_counter=0;
    pthread_mutex_unlock(&table_lock);
}
__attribute__((constructor)) static void initialize(void) {
    const char *url=getenv("CENSORPRIVACY_URL"), *token=getenv("CENSORPRIVACY_TOKEN");
    const char *peer=getenv("CENSORPRIVACY_TLS_PEER");
    if (!url || strncmp(url,"http://",7)!=0 || !token || strlen(token)<24 || strlen(token)>512)
        fatal("configuration_invalid");
    char endpoint[81];
    const char *authority=url+7;
    size_t length=strlen(authority);
    if (length && authority[length-1]=='/') length--;
    if (!length||length>=sizeof(endpoint)) fatal("configuration_invalid");
    memcpy(endpoint,authority,length); endpoint[length]=0;
    if (!parse_endpoint(endpoint,&broker_address,true,broker_host)
        || !parse_endpoint(peer,&peer_address,false,NULL)) fatal("configuration_invalid");
    for (size_t i=0;token[i];i++) if ((unsigned char)token[i]<33 || (unsigned char)token[i]>126)
        fatal("configuration_invalid");
    strcpy(access_token,token);
    seed_streams();
    if (!seed_valid || pthread_atfork(before_fork,after_fork_parent,after_fork_child)!=0)
        fatal("initialization_failed");
    configured=true;
}
static void resolve_backend(void) {
#define LOAD_REQUIRED(field,name) do { void *symbol=dlsym(RTLD_NEXT,name); \
    if (!symbol || sizeof(api.field)!=sizeof(symbol)) fatal("symbols_unavailable"); \
    memcpy(&api.field,&symbol,sizeof(symbol)); } while(0)
    LOAD_REQUIRED(write,"SSL_write"); LOAD_REQUIRED(write_ex,"SSL_write_ex");
    LOAD_REQUIRED(read,"SSL_read"); LOAD_REQUIRED(read_ex,"SSL_read_ex");
    LOAD_REQUIRED(get_error,"SSL_get_error"); LOAD_REQUIRED(get_fd,"SSL_get_fd");
    LOAD_REQUIRED(verify_mode,"SSL_get_verify_mode"); LOAD_REQUIRED(verify_result,"SSL_get_verify_result");
    LOAD_REQUIRED(handshake_finished,"SSL_is_init_finished");
    void *optional=dlsym(RTLD_NEXT,"SSL_connect");
    if(optional && sizeof(api.connect)==sizeof(optional)) memcpy(&api.connect,&optional,sizeof(optional));
    optional=dlsym(RTLD_NEXT,"SSL_do_handshake");
    if(optional && sizeof(api.handshake)==sizeof(optional)) memcpy(&api.handshake,&optional,sizeof(optional));
    LOAD_REQUIRED(ctrl,"SSL_ctrl"); LOAD_REQUIRED(new_ssl,"SSL_new");
    LOAD_REQUIRED(up_ref,"SSL_up_ref");
    LOAD_REQUIRED(free_ssl,"SSL_free"); LOAD_REQUIRED(pending,"SSL_pending");
    void *symbol=dlsym(RTLD_NEXT,"SSL_has_pending");
    if (symbol && sizeof(api.has_pending)==sizeof(symbol)) memcpy(&api.has_pending,&symbol,sizeof(symbol));
#undef LOAD_REQUIRED
}
static void ensure_backend(void) {
    if (!configured) fatal("configuration_invalid");
    if (pthread_once(&backend_once,resolve_backend)!=0) fatal("symbols_unavailable");
}
static void forget_inherited(SSL *ssl) {
    for (size_t i=0;i<inherited_count;i++) if(inherited_connections[i]==ssl) {
        inherited_count--;
        inherited_connections[i]=inherited_connections[inherited_count];
        inherited_references[i]=inherited_references[inherited_count]; return;
    }
}
static struct state *state_get(SSL *ssl, bool create) {
    if (!ssl) return NULL;
    pthread_mutex_lock(&table_lock);
    for (size_t i=0;i<inherited_count;i++) if(inherited_connections[i]==ssl) {
        pthread_mutex_unlock(&table_lock); return NULL;
    }
    struct state *item=states;
    while(item && item->ssl!=ssl) item=item->next;
    if (!item && create && state_count<MAX_STATES) {
        if (!seed_valid) seed_streams();
        if (seed_valid && stream_counter!=UINT64_MAX) {
            item=calloc(1,sizeof(*item));
            if (item && pthread_mutex_init(&item->lock,NULL)==0) {
                item->ssl=ssl; item->owner=getpid(); item->fd=item->bound_fd=-1; item->tls_references=1;
                do { item->stream=stream_seed+(++stream_counter); } while(!item->stream && stream_counter!=UINT64_MAX);
                item->next=states; states=item; state_count++;
            } else { free(item); item=NULL; }
        }
    }
    if (item && !item->closing) item->references++; else item=NULL;
    pthread_mutex_unlock(&table_lock);
    return item;
}
static void write_clear(struct state *item) {
    if(item->write.original) { erase(item->write.original,item->write.original_length); free(item->write.original); }
    if(item->write.allocation) { erase(item->write.allocation,item->write.approved_length+RESP_HEADER); free(item->write.allocation); }
    release_memory(item->write.reservation);
    memset(&item->write,0,sizeof(item->write));
}
static void read_clear(struct state *item) {
    if(item->safe_allocation) { erase(item->safe_allocation,item->safe_length+RESP_HEADER); free(item->safe_allocation); }
    release_memory(item->safe_reservation);
    item->safe_allocation=item->safe=NULL;
    item->safe_length=item->safe_offset=item->safe_reservation=0;
    erase(item->stage,sizeof(item->stage));
}
static void state_put(struct state *item) {
    if (item->owner!=getpid()) return;
    bool destroy=false;
    pthread_mutex_lock(&table_lock);
    if (--item->references==0 && item->closing) destroy=true;
    pthread_mutex_unlock(&table_lock);
    if(destroy) { pthread_mutex_destroy(&item->lock); free(item); }
}
static bool socket_matches_peer(int fd) {
    int type=0, protocol=0;
    socklen_t type_length=sizeof(type), length=sizeof(struct sockaddr_in);
    struct sockaddr_in actual;
    if(fd<0 || getsockopt(fd,SOL_SOCKET,SO_TYPE,&type,&type_length)<0 || type!=SOCK_STREAM
       || getsockopt(fd,SOL_SOCKET,SO_PROTOCOL,&protocol,&type_length)<0 || protocol!=IPPROTO_TCP)
        return false;
    if(getpeername(fd,(struct sockaddr*)&actual,&length)<0 || length!=sizeof(actual)
       || actual.sin_family!=AF_INET || actual.sin_addr.s_addr!=peer_address.sin_addr.s_addr
       || actual.sin_port!=peer_address.sin_port) return false;
    return true;
}
static bool verify_peer(struct state *item) {
    if(item->binding_ambiguous) return peer_failure("peer_binding_ambiguous");
    int fd=api.get_fd(item->ssl);
    if(fd<0) fd=item->bound_fd;
    if(fd<0) return peer_failure("peer_fd_unavailable");
    if(!api.handshake_finished(item->ssl)) return peer_failure("handshake_not_finished");
    if(!(api.verify_mode(item->ssl)&SSL_VERIFY_PEER)) return peer_failure("verification_disabled");
    if(api.verify_result(item->ssl)!=X509_V_OK) return peer_failure("verification_failed");
    if(!socket_matches_peer(fd)) return peer_failure("peer_mismatch");
    if(item->bound_fd>=0 && item->bound_fd!=fd) return peer_failure("peer_binding_ambiguous");
    if(item->peer_verified && item->fd!=fd) return peer_failure("peer_fd_changed");
    item->fd=fd; item->peer_verified=true; return true;
}
static void resolve_socket_backend(void) {
    void *symbol=dlsym(RTLD_NEXT,"send");
    if(!symbol||sizeof(real_send)!=sizeof(symbol)) fatal("socket_symbols_unavailable");
    memcpy(&real_send,&symbol,sizeof(symbol));
    symbol=dlsym(RTLD_NEXT,"recv");
    if(!symbol||sizeof(real_recv)!=sizeof(symbol)) fatal("socket_symbols_unavailable");
    memcpy(&real_recv,&symbol,sizeof(symbol));
}
static bool observe_io(int fd) {
    struct io_context *context=active_io;
    if(rpc_depth||!context||context->owner!=getpid()) return true;
    if(!socket_matches_peer(fd)) { context->ambiguous=true; errno=EPERM; return false; }
    if(context->observed_fd>=0 && context->observed_fd!=fd) { context->ambiguous=true; errno=EPERM; return false; }
    if(!context->handshake && context->item->fd!=fd) { context->ambiguous=true; errno=EPERM; return false; }
    context->observed_fd=fd; return true;
}
ssize_t send(int fd,const void *buffer,size_t length,int flags) {
    int previous=errno;
    if(pthread_once(&socket_once,resolve_socket_backend)!=0) fatal("socket_symbols_unavailable");
    if(!observe_io(fd)) return -1;
    errno=previous; return real_send(fd,buffer,length,flags);
}
ssize_t recv(int fd,void *buffer,size_t length,int flags) {
    int previous=errno;
    if(pthread_once(&socket_once,resolve_socket_backend)!=0) fatal("socket_symbols_unavailable");
    if(!observe_io(fd)) return -1;
    errno=previous; return real_recv(fd,buffer,length,flags);
}
static void push_io(struct io_context *context,struct state *item,bool handshake) {
    *context=(struct io_context){.ssl=item->ssl,.item=item,.parent=active_io,.owner=getpid(),
                                .observed_fd=-1,.handshake=handshake};
    active_io=context;
}
static bool pop_io(struct io_context *context) {
    if(context->owner!=getpid()) { active_io=NULL; return false; }
    active_io=context->parent;
    if(context->ambiguous) context->item->binding_ambiguous=true;
    if(context->observed_fd>=0) {
        if(context->item->bound_fd>=0 && context->item->bound_fd!=context->observed_fd)
            context->item->binding_ambiguous=true;
        else context->item->bound_fd=context->observed_fd;
    }
    return true;
}
static int handshake_common(SSL *ssl,bool connect) {
    ensure_backend();
    int (*original)(SSL*)=connect?api.connect:api.handshake;
    if(!original) fatal("symbols_unavailable");
    if(rpc_depth) return fail(EPERM);
    bool nested=active_io&&active_io->ssl==ssl&&active_io->handshake;
    struct state *item=nested?active_io->item:state_get(ssl,true);
    if(!item) return fail(EPERM);
    if(!nested && pthread_mutex_trylock(&item->lock)!=0) { state_put(item); return fail(EBUSY); }
    if(item->closing||item->poisoned) { if(!nested) { pthread_mutex_unlock(&item->lock); state_put(item); } return fail(EPERM); }
    struct io_context context; push_io(&context,item,true);
    int result=original(ssl), preserved=errno;
    (void)api.get_error(ssl,result); errno=preserved;
    if(!pop_io(&context)) return fail(EPERM);
    if(!nested) { pthread_mutex_unlock(&item->lock); state_put(item); }
    errno=preserved; return result;
}
int SSL_connect(SSL *ssl) { return handshake_common(ssl,true); }
int SSL_do_handshake(SSL *ssl) { return handshake_common(ssl,false); }
static struct state *enter(SSL *ssl) {
    ensure_backend();
    if(rpc_depth) { fail(EPERM); return NULL; }
    struct state *item=state_get(ssl,true);
    if(!item) { fail(EPERM); return NULL; }
    if(pthread_mutex_trylock(&item->lock)!=0) { state_put(item); fail(EBUSY); return NULL; }
    if(item->closing || item->poisoned || !verify_peer(item)) {
        item->poisoned=true; pthread_mutex_unlock(&item->lock); state_put(item); fail(EPERM); return NULL;
    }
    return item;
}
static void leave(struct state *item) { pthread_mutex_unlock(&item->lock); state_put(item); }
static int64_t monotonic_ms(void) {
    struct timespec now;
    if(clock_gettime(CLOCK_MONOTONIC,&now)<0) return -1;
    return (int64_t)now.tv_sec*1000+now.tv_nsec/1000000;
}
static bool wait_fd(int fd, short event, int64_t deadline) {
    for(;;) {
        int64_t now=monotonic_ms();
        if(now<0 || now>=deadline) return false;
        struct pollfd descriptor={.fd=fd,.events=event};
        int result=poll(&descriptor,1,(int)(deadline-now));
        if(result<0 && errno==EINTR) continue;
        if(result<=0) return false;
        return (descriptor.revents&event)!=0;
    }
}
static bool send_all(int fd,const void *data,size_t length,int64_t deadline) {
    const unsigned char *bytes=data;
    while(length) {
        int64_t now=monotonic_ms();
        if(now<0||now>=deadline) return false;
        ssize_t amount=send(fd,bytes,length,MSG_NOSIGNAL);
        if(amount>0) { bytes+=amount; length-=(size_t)amount; continue; }
        if(amount<0 && errno==EINTR) continue;
        if(amount<0 && (errno==EAGAIN||errno==EWOULDBLOCK) && wait_fd(fd,POLLOUT,deadline)) continue;
        return false;
    }
    return true;
}
static bool receive_all(int fd,void *data,size_t length,int64_t deadline) {
    unsigned char *bytes=data;
    while(length) {
        int64_t now=monotonic_ms();
        if(now<0||now>=deadline) return false;
        ssize_t amount=recv(fd,bytes,length,0);
        if(amount>0) { bytes+=amount; length-=(size_t)amount; continue; }
        if(amount<0 && errno==EINTR) continue;
        if(amount<0 && (errno==EAGAIN||errno==EWOULDBLOCK) && wait_fd(fd,POLLIN,deadline)) continue;
        return false;
    }
    return true;
}
static void put32(unsigned char *output,uint32_t value) {
    for(unsigned i=0;i<4;i++) output[i]=(unsigned char)(value>>(24-i*8));
}
static void put64(unsigned char *output,uint64_t value) {
    for(unsigned i=0;i<8;i++) output[i]=(unsigned char)(value>>(56-i*8));
}
static uint32_t get32(const unsigned char *bytes) {
    return ((uint32_t)bytes[0]<<24)|((uint32_t)bytes[1]<<16)|((uint32_t)bytes[2]<<8)|bytes[3];
}
struct reply { unsigned action; size_t length; unsigned char *allocation,*payload; };
static bool parse_http_header(char *header,size_t *length) {
    char *line=strstr(header,"\r\n");
    if(!line) return false;
    *line=0;
    if((strncmp(header,"HTTP/1.1 200",12)!=0 && strncmp(header,"HTTP/1.0 200",12)!=0)
       || (header[12]!=' '&&header[12]!=0)) return false;
    bool have_length=false, have_type=false;
    char *cursor=line+2;
    while(*cursor) {
        line=strstr(cursor,"\r\n");
        if(!line) return false;
        *line=0;
        if(!*cursor) break;
        char *colon=strchr(cursor,':'); if(!colon||colon==cursor) return false;
        *colon++=0; while(*colon==' '||*colon=='\t') colon++;
        char *end=colon+strlen(colon); while(end>colon&&(end[-1]==' '||end[-1]=='\t')) *--end=0;
        if(strcasecmp(cursor,"Transfer-Encoding")==0) return false;
        if(strcasecmp(cursor,"Content-Length")==0) {
            if(have_length||!*colon) return false;
            size_t number=0;
            for(const char *p=colon;*p;p++) {
                if(*p<'0'||*p>'9'||number>(MAX_PAYLOAD+RESP_HEADER)/10) return false;
                number=number*10+(size_t)(*p-'0');
                if(number>MAX_PAYLOAD+RESP_HEADER) return false;
            }
            *length=number; have_length=true;
        } else if(strcasecmp(cursor,"Content-Type")==0) {
            if(have_type||strcasecmp(colon,"application/octet-stream")!=0) return false;
            have_type=true;
        }
        cursor=line+2;
    }
    return have_length&&have_type&&*length>=RESP_HEADER;
}
static bool rpc(unsigned direction,uint64_t stream,const unsigned char *payload,size_t length,
                size_t expected,struct reply *reply) {
    memset(reply,0,sizeof(*reply));
    if(!length||length>MAX_PAYLOAD||expected>MAX_PAYLOAD) return false;
    int64_t start=monotonic_ms(); if(start<0) return false;
    int64_t deadline=start+RPC_TIMEOUT_MS;
    if(rpc_depth) return false;
    rpc_depth++;
    int fd=socket(AF_INET,SOCK_STREAM|SOCK_NONBLOCK|SOCK_CLOEXEC,0);
    if(fd<0) { rpc_depth--; return false; }
    bool success=false;
    unsigned char *body=NULL;
    size_t body_length=0;
    if(connect(fd,(struct sockaddr*)&broker_address,sizeof(broker_address))<0) {
        if(errno!=EINPROGRESS||!wait_fd(fd,POLLOUT,deadline)) goto done;
        int error=0; socklen_t error_length=sizeof(error);
        if(getsockopt(fd,SOL_SOCKET,SO_ERROR,&error,&error_length)<0||error) goto done;
    }
    char header[1024];
    int header_length=snprintf(header,sizeof(header),
        "POST /v1/tls/%s HTTP/1.1\r\nHost: %s\r\nAuthorization: Bearer %s\r\n"
        "Content-Type: application/octet-stream\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n",
        direction==1?"out":"in",broker_host,access_token,length+REQ_HEADER);
    if(header_length<=0||(size_t)header_length>=sizeof(header)) goto done;
    unsigned char request[REQ_HEADER]={ 'C','P','T','1',0,0,0,0 };
    request[4]=(unsigned char)direction; put64(request+8,stream); put32(request+16,(uint32_t)length);
    if(!send_all(fd,header,(size_t)header_length,deadline)||!send_all(fd,request,sizeof(request),deadline)
        ||!send_all(fd,payload,length,deadline)) goto done;
    char response_header[MAX_HEADER+1]; size_t position=0;
    for(;;) {
        if(position==MAX_HEADER||!receive_all(fd,response_header+position,1,deadline)) goto done;
        position++; response_header[position]=0;
        if(position>=4 && memcmp(response_header+position-4,"\r\n\r\n",4)==0) break;
        if(response_header[position-1]==0) goto done;
    }
    if(!parse_http_header(response_header,&body_length)||body_length>expected+RESP_HEADER) goto done;
    body=malloc(body_length); if(!body) goto done;
    if(!receive_all(fd,body,body_length,deadline)||memcmp(body,"CPR1",4)!=0
       ||body[5]||body[6]||body[7]) goto done;
    size_t result_length=get32(body+8); unsigned action=body[4];
    if(result_length+RESP_HEADER!=body_length) goto done;
    if(action==ACT_DEFER||action==ACT_BLOCK) { if(result_length) goto done; }
    else if(action==ACT_REPLACE) { if(result_length!=expected) goto done; }
    else if(action==ACT_ALLOW) {
        if(expected!=length||result_length!=length||!same_bytes(body+RESP_HEADER,payload,length)) goto done;
    } else goto done;
    reply->action=action; reply->length=result_length; reply->allocation=body; reply->payload=body+RESP_HEADER;
    body=NULL; success=true;
done:
    if(body) { erase(body,body_length); free(body); }
    close(fd); if(rpc_depth) rpc_depth--; return success;
}
static int write_common(SSL *ssl,const void *buffer,size_t length,size_t *written,int kind) {
    if(written) *written=0;
    if(!buffer||!length||length>MAX_PAYLOAD) return fail(EPERM);
    struct state *item=enter(ssl); if(!item) return -1;
    int result=-1, saved_errno=errno;
    if(item->write.original) {
        if(kind!=item->write.kind || length!=item->write.original_length
           || (!item->write.moving && buffer!=item->write.caller_pointer)
           || !same_bytes(buffer,item->write.original,length)) goto blocked;
    } else {
        if(item->awaiting_response||length>MAX_PAYLOAD-item->out_held) goto blocked;
        size_t expected=item->out_held+length, reservation=length+expected+RESP_HEADER;
        if(!reserve_memory(reservation)) goto blocked;
        item->write.reservation=reservation; item->write.original_length=length;
        item->write.original=malloc(length);
        if(!item->write.original) goto blocked;
        memcpy(item->write.original,buffer,length);
        struct reply reply;
        bool ok=rpc(1,item->stream,item->write.original,length,expected,&reply);
        if(item->owner!=getpid()) {
            if(reply.allocation) { erase(reply.allocation,reply.length+RESP_HEADER); free(reply.allocation); }
            return fail(EPERM);
        }
        if(!ok) goto blocked;
        if(reply.action==ACT_DEFER) {
            erase(reply.allocation,RESP_HEADER); free(reply.allocation);
            item->out_held=expected; write_clear(item); result=1; goto completed;
        }
        if(reply.action==ACT_BLOCK) { erase(reply.allocation,RESP_HEADER); free(reply.allocation); goto blocked; }
        item->write.allocation=reply.allocation; item->write.approved=reply.payload;
        item->write.approved_length=reply.length;
        item->write.caller_pointer=buffer; item->write.kind=kind;
        item->write.moving=(api.ctrl(ssl,SSL_CTRL_MODE,0,NULL)&SSL_MODE_ACCEPT_MOVING_WRITE_BUFFER)!=0;
        item->out_held=0;
    }
    while(item->write.offset<item->write.approved_length) {
        size_t remaining=item->write.approved_length-item->write.offset, accepted=0;
        const unsigned char *pointer=item->write.approved+item->write.offset;
        struct io_context context; push_io(&context,item,false);
        int raw=kind==1?api.write(ssl,pointer,(int)remaining):api.write_ex(ssl,pointer,remaining,&accepted);
        saved_errno=errno;
        int error=api.get_error(ssl,raw); errno=saved_errno;
        if(!pop_io(&context)||item->owner!=getpid()) return fail(EPERM);
        if(item->binding_ambiguous) goto blocked;
        if((kind==1&&raw>0)||(kind==2&&raw==1)) {
            if(kind==1) accepted=(size_t)raw;
            if(!accepted||accepted>remaining) goto blocked;
            item->write.offset+=accepted;
        } else if(error==SSL_ERROR_WANT_READ||error==SSL_ERROR_WANT_WRITE) {
            result=raw; goto finished;
        } else { item->poisoned=true; write_clear(item); result=raw; goto finished; }
    }
    write_clear(item); item->awaiting_response=true; result=1;
completed:
    if(written) *written=length;
    result=kind==1?(int)length:1;
    goto finished;
blocked:
    item->poisoned=true; write_clear(item); fail(EPERM); saved_errno=errno; result=-1;
finished:
    leave(item); errno=saved_errno; return result;
}
static int read_common(SSL *ssl,void *buffer,size_t length,size_t *received,int kind) {
    if(received) *received=0;
    if(!buffer||!length||length>MAX_PAYLOAD) return fail(EPERM);
    struct state *item=enter(ssl); if(!item) { erase(buffer,length); return -1; }
    int result=-1, saved_errno=errno;
    if(item->write.original || (item->read_wait_kind && item->read_wait_kind!=kind)) goto blocked;
    while(!item->safe) {
        size_t count=0;
        struct io_context context; push_io(&context,item,false);
        int raw=kind==1?api.read(ssl,item->stage,READ_STAGE):api.read_ex(ssl,item->stage,READ_STAGE,&count);
        saved_errno=errno;
        int error=api.get_error(ssl,raw); errno=saved_errno;
        if(!pop_io(&context)||item->owner!=getpid()) return fail(EPERM);
        if(item->binding_ambiguous) goto blocked;
        if(!((kind==1&&raw>0)||(kind==2&&raw==1))) {
            erase(item->stage,sizeof(item->stage));
            if(error==SSL_ERROR_WANT_READ||error==SSL_ERROR_WANT_WRITE) {
                item->read_wait_kind=kind; result=raw; goto finished;
            }
            if(item->in_held) goto blocked;
            result=raw; goto finished;
        }
        item->read_wait_kind=0; if(kind==1) count=(size_t)raw;
        if(!count||count>READ_STAGE||count>MAX_PAYLOAD-item->in_held) goto blocked;
        size_t expected=item->in_held+count, reservation=expected+RESP_HEADER;
        if(!reserve_memory(reservation)) goto blocked;
        struct reply reply;
        bool ok=rpc(2,item->stream,item->stage,count,expected,&reply);
        if(item->owner!=getpid()) {
            if(reply.allocation) { erase(reply.allocation,reply.length+RESP_HEADER); free(reply.allocation); }
            erase(buffer,length); return fail(EPERM);
        }
        erase(item->stage,sizeof(item->stage));
        if(!ok) { release_memory(reservation); goto blocked; }
        if(reply.action==ACT_BLOCK) { erase(reply.allocation,RESP_HEADER); free(reply.allocation); release_memory(reservation); goto blocked; }
        if(reply.action==ACT_DEFER) {
            erase(reply.allocation,RESP_HEADER); free(reply.allocation); release_memory(reservation);
            item->in_held=expected; continue;
        }
        item->safe_allocation=reply.allocation; item->safe=reply.payload;
        item->safe_length=reply.length; item->safe_reservation=reservation; item->in_held=0;
    }
    size_t count=item->safe_length-item->safe_offset;
    if(count>length) count=length;
    memcpy(buffer,item->safe+item->safe_offset,count); item->safe_offset+=count;
    if(item->safe_offset==item->safe_length) { read_clear(item); item->awaiting_response=false; }
    if(received) *received=count;
    result=kind==1?(int)count:1; goto finished;
blocked:
    item->poisoned=true; read_clear(item); erase(buffer,length); fail(EPERM); saved_errno=errno; result=-1;
finished:
    leave(item); errno=saved_errno; return result;
}

int SSL_write(SSL *ssl,const void *buffer,int length) {
    return write_common(ssl,buffer,length>0?(size_t)length:0,NULL,1);
}
int SSL_write_ex(SSL *ssl,const void *buffer,size_t length,size_t *written) {
    if(!written) { fail(EPERM); return 0; }
    int result=write_common(ssl,buffer,length,written,2);
    return result==1?1:0;
}
int SSL_read(SSL *ssl,void *buffer,int length) {
    return read_common(ssl,buffer,length>0?(size_t)length:0,NULL,1);
}
int SSL_read_ex(SSL *ssl,void *buffer,size_t length,size_t *received) {
    if(!received) { fail(EPERM); return 0; }
    int result=read_common(ssl,buffer,length,received,2);
    return result==1?1:0;
}
static int unsupported_entry(SSL *ssl) {
    ensure_backend();
    struct state *item=state_get(ssl,false);
    if(item) {
        if(pthread_mutex_trylock(&item->lock)==0) {
            item->poisoned=true; write_clear(item); read_clear(item); leave(item);
        } else state_put(item);
    }
    (void)peer_failure("unsupported_tls_entry");
    return fail(EPERM);
}
int SSL_write_early_data(SSL *ssl,const void *buffer,size_t length,size_t *written) {
    (void)buffer; (void)length; if(written) *written=0;
    unsupported_entry(ssl); return 0;
}
int SSL_write_ex2(SSL *ssl,const void *buffer,size_t length,uint64_t flags,size_t *written) {
    (void)buffer; (void)length; (void)flags; if(written) *written=0;
    unsupported_entry(ssl); return 0;
}
ssize_t SSL_sendfile(SSL *ssl,int fd,off_t offset,size_t size,int flags) {
    (void)fd; (void)offset; (void)size; (void)flags;
    return unsupported_entry(ssl);
}
int SSL_peek(SSL *ssl,void *buffer,int length) {
    if(buffer&&length>0&&(size_t)length<=MAX_PAYLOAD) erase(buffer,(size_t)length);
    return unsupported_entry(ssl);
}
int SSL_peek_ex(SSL *ssl,void *buffer,size_t length,size_t *received) {
    if(received) *received=0;
    if(buffer&&length<=MAX_PAYLOAD) erase(buffer,length);
    unsupported_entry(ssl); return 0;
}
int SSL_read_early_data(SSL *ssl,void *buffer,size_t length,size_t *received) {
    if(received) *received=0;
    if(buffer&&length<=MAX_PAYLOAD) erase(buffer,length);
    unsupported_entry(ssl); return 0;
}
SSL *SSL_new(SSL_CTX *context) {
    ensure_backend(); SSL *ssl=api.new_ssl(context);
    if(ssl) { pthread_mutex_lock(&table_lock); forget_inherited(ssl); pthread_mutex_unlock(&table_lock); }
    if(ssl) {
        struct state *item=state_get(ssl,true);
        if(!item) { api.free_ssl(ssl); fail(EPERM); return NULL; }
        state_put(item);
    }
    return ssl;
}
int SSL_up_ref(SSL *ssl) {
    ensure_backend();
    struct state *item=state_get(ssl,true);
    if(!item) { fail(EPERM); return 0; }
    if(pthread_mutex_trylock(&item->lock)!=0) { state_put(item); fail(EBUSY); return 0; }
    int result=0, saved_errno=errno;
    if(item->closing||item->tls_references==UINT_MAX) { fail(EPERM); saved_errno=errno; }
    else {
        result=api.up_ref(ssl); saved_errno=errno;
        if(result==1) item->tls_references++;
    }
    leave(item); errno=saved_errno; return result;
}
void SSL_free(SSL *ssl) {
    if(!ssl) return;
    ensure_backend();
    pthread_mutex_lock(&table_lock);
    bool inherited=false;
    for(size_t i=0;i<inherited_count;i++) if(inherited_connections[i]==ssl) {
        inherited=true;
        if(inherited_references[i]>1) inherited_references[i]--;
        else forget_inherited(ssl);
        break;
    }
    pthread_mutex_unlock(&table_lock);
    if(inherited) { api.free_ssl(ssl); return; }
    struct state *item=state_get(ssl,false);
    if(!item) {
        pthread_mutex_lock(&table_lock);
        struct state *known=states;
        while(known&&known->ssl!=ssl) known=known->next;
        bool closing=known&&known->closing;
        pthread_mutex_unlock(&table_lock);
        if(closing) { fail(EBUSY); return; }
        api.free_ssl(ssl); return;
    }
    if(pthread_mutex_trylock(&item->lock)!=0) { state_put(item); fail(EBUSY); return; }
    if(item->closing) { leave(item); fail(EBUSY); return; }
    if(item->tls_references>1) { item->tls_references--; api.free_ssl(ssl); leave(item); return; }
    pthread_mutex_lock(&table_lock); item->closing=true; pthread_mutex_unlock(&table_lock);
    write_clear(item); read_clear(item); api.free_ssl(ssl);
    pthread_mutex_lock(&table_lock);
    struct state **cursor=&states; while(*cursor&&*cursor!=item) cursor=&(*cursor)->next;
    if(*cursor) { *cursor=item->next; state_count--; }
    pthread_mutex_unlock(&table_lock);
    leave(item);
}
static int pending_common(const SSL *ssl,bool has) {
    ensure_backend(); struct state *item=state_get((SSL*)ssl,false);
    if(!item) {
        pthread_mutex_lock(&table_lock);
        bool unavailable=false;
        for(size_t i=0;i<inherited_count;i++) if(inherited_connections[i]==ssl) unavailable=true;
        for(struct state *known=states;known;known=known->next) if(known->ssl==ssl&&known->closing) unavailable=true;
        pthread_mutex_unlock(&table_lock);
        if(unavailable) { fail(EPERM); return 0; }
        return has&&api.has_pending?api.has_pending(ssl):has?api.pending(ssl)>0:api.pending(ssl);
    }
    if(pthread_mutex_trylock(&item->lock)!=0) { state_put(item); fail(EBUSY); return 0; }
    size_t ready=item->safe_length-item->safe_offset;
    int result=item->poisoned||item->closing?0:ready?(has?1:ready>INT_MAX?INT_MAX:(int)ready)
        :has&&api.has_pending?api.has_pending(ssl):has?api.pending(ssl)>0:api.pending(ssl);
    leave(item); return result;
}
int SSL_pending(const SSL *ssl) { return pending_common(ssl,false); }
int SSL_has_pending(const SSL *ssl) { return pending_common(ssl,true); }
