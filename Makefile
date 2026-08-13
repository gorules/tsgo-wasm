TSGO_REPO = https://github.com/microsoft/typescript-go.git
TSGO_REV ?= $(shell cat artifacts/tsgo.rev)

tsgo:
	rm -rf .tsgo-build
	git init -q .tsgo-build
	git -C .tsgo-build remote add origin $(TSGO_REPO)
	git -C .tsgo-build fetch -q --depth 1 origin $(TSGO_REV)
	git -C .tsgo-build checkout -q FETCH_HEAD
	cd .tsgo-build && GOOS=wasip1 GOARCH=wasm go build -o ../artifacts/tsgo.wasm ./cmd/tsgo
	zstd -19 -T0 -q -f --rm -o artifacts/tsgo.wasm.zst artifacts/tsgo.wasm
	git -C .tsgo-build rev-parse HEAD | tr -d '\n' > artifacts/tsgo.rev
	shasum -a 256 artifacts/tsgo.wasm.zst | cut -d' ' -f1 | tr -d '\n' > artifacts/tsgo.sha256
	rm -rf .tsgo-build

test:
	cargo test --release

upload:
	$(eval REV10 := $(shell cut -c1-10 artifacts/tsgo.rev))
	cp artifacts/tsgo.wasm.zst tsgo-$(REV10).wasm.zst
	gh release create tsgo-$(REV10) --prerelease \
		--notes "tsgo module @ microsoft/typescript-go@$$(cat artifacts/tsgo.rev). sha256: $$(cat artifacts/tsgo.sha256)" \
		tsgo-$(REV10).wasm.zst \
		|| gh release upload tsgo-$(REV10) tsgo-$(REV10).wasm.zst --clobber
	rm tsgo-$(REV10).wasm.zst

update: tsgo test upload
