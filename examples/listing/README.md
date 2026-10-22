# listing

A browser artifact that lists a directory, refused at compile time because a page has no filesystem.

```
cd examples/listing
vyrn check client/boot.vyrn
```

In a browser `listDir` would answer `Err` and the page would print an error where a listing should be. [client/boot.vyrn](client/boot.vyrn) is refused by the capability floor instead: `listDir` needs `fs`, and the `browser` target has none.
