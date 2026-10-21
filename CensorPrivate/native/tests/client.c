#define _POSIX_C_SOURCE 200809L
#include <openssl/ssl.h>
#include <openssl/x509v3.h>
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>
/* A real dynamic OpenSSL client; fixed diagnostics contain no payload. */
static void failed(unsigned line) { fprintf(stderr,"fixture_failed_at_%u\n",line); exit(2); }
#define fail() failed(__LINE__)
static void wait_tls(int fd,int error) {
    struct pollfd p={.fd=fd,.events=error==SSL_ERROR_WANT_READ?POLLIN:POLLOUT};
    if(poll(&p,1,10000)<=0) fail();
}
int main(int argc,char **argv) {
    if(argc!=6) fail(); /* port, CA, full HTTP request, API kind, fragment width */
    int port=atoi(argv[1]),kind=atoi(argv[4]),api_kind=kind==2?2:1;size_t width=(size_t)strtoul(argv[5],NULL,10);
    FILE *file=fopen(argv[3],"rb"); if(!file||fseek(file,0,SEEK_END)) fail();
    long size=ftell(file); if(size<=0||size>300000||fseek(file,0,SEEK_SET)) fail();
    unsigned char *request=malloc((size_t)size); if(!request||fread(request,1,(size_t)size,file)!=(size_t)size) fail();fclose(file);
    int fd=socket(AF_INET,SOCK_STREAM,0);struct sockaddr_in peer={.sin_family=AF_INET,.sin_port=htons((unsigned short)port)};peer.sin_addr.s_addr=htonl(INADDR_LOOPBACK);
    if(fd<0||connect(fd,(struct sockaddr*)&peer,sizeof(peer))) fail();
    SSL_CTX *context=SSL_CTX_new(TLS_client_method());if(!context||SSL_CTX_set_min_proto_version(context,TLS1_2_VERSION)!=1||SSL_CTX_load_verify_locations(context,argv[2],NULL)!=1) fail();
    SSL_CTX_set_verify(context,SSL_VERIFY_PEER,NULL);SSL *ssl=SSL_new(context);if(!ssl||SSL_set_fd(ssl,fd)!=1||X509_VERIFY_PARAM_set1_ip_asc(SSL_get0_param(ssl),"127.0.0.1")!=1||SSL_connect(ssl)!=1) fail();
    SSL_set_mode(ssl,SSL_MODE_ENABLE_PARTIAL_WRITE|SSL_MODE_ACCEPT_MOVING_WRITE_BUFFER);
    int small=1024;if(setsockopt(fd,SOL_SOCKET,SO_SNDBUF,&small,sizeof(small))||fcntl(fd,F_SETFL,O_NONBLOCK)) fail();
    unsigned retries=0;size_t position=0;
    while(position<(size_t)size) {
        size_t length=(size_t)size-position;if(width&&length>width) length=width;
        unsigned char *copy=malloc(length);if(!copy) fail();memcpy(copy,request+position,length);
        for(;;) {
            size_t accepted=0;int result=api_kind==1?SSL_write(ssl,copy,(int)length):SSL_write_ex(ssl,copy,length,&accepted);
            if((api_kind==1&&result>0)||(api_kind==2&&result==1)) {if(api_kind==1) accepted=(size_t)result;if(!accepted||accepted!=length) fail();position+=accepted;break;}
            int error=SSL_get_error(ssl,result);if(error!=SSL_ERROR_WANT_READ&&error!=SSL_ERROR_WANT_WRITE) { fprintf(stderr,"write_rejected_position=%zu_error=%d\n",position,error); fail(); }
            retries++;wait_tls(fd,error);
            if(kind==3) {
                copy[0]^=1;
                int changed=SSL_write(ssl,copy,(int)length);
                int changed_error=SSL_get_error(ssl,changed);
                if(changed>0 || changed_error==SSL_ERROR_WANT_READ || changed_error==SSL_ERROR_WANT_WRITE) fail();
                fputs("retry_mutation_blocked\n",stderr);
                free(copy);free(request);SSL_free(ssl);SSL_CTX_free(context);close(fd);return 0;
            }
            unsigned char *moved=malloc(length);if(!moved) fail();memcpy(moved,copy,length);free(copy);copy=moved;
        }free(copy);
    }
    if(kind==3) fail(); /* Mutation test must actually reach a pending retry. */
    free(request);unsigned char buffer[137];size_t total=0;
    for(;;) {
        size_t received=0;int result=kind==1?SSL_read(ssl,buffer,sizeof(buffer)):SSL_read_ex(ssl,buffer,sizeof(buffer),&received);
        if((kind==1&&result>0)||(kind==2&&result==1)) {if(kind==1) received=(size_t)result;if(fwrite(buffer,1,received,stdout)!=received) fail();total+=received;continue;}
        int error=SSL_get_error(ssl,result);if(error==SSL_ERROR_WANT_READ||error==SSL_ERROR_WANT_WRITE) {wait_tls(fd,error);continue;}
        if(total) break;
        fail();
    }
    fprintf(stderr,"retries=%u\n",retries);SSL_free(ssl);SSL_CTX_free(context);close(fd);return 0;
}
