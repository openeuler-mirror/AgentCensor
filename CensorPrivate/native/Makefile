CC ?= cc
CPPFLAGS += $(shell pkg-config --cflags openssl 2>/dev/null)
CFLAGS ?= -O2 -g
CFLAGS += -std=c11 -Wall -Wextra -Werror -fPIC -fvisibility=hidden
LDLIBS += -lcrypto -ldl -pthread

all: libcp_tls_guard.so

libcp_tls_guard.so: tls_guard.c
	$(CC) $(CPPFLAGS) $(CFLAGS) -fvisibility=default -shared -Wl,-z,relro,-z,now -o $@ $< $(LDLIBS)

clean:
	rm -f libcp_tls_guard.so

.PHONY: all clean
