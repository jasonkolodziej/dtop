/// Private keys generated with OpenSSL 3 (`genrsa -traditional`, `ecparam -genkey`, `pkcs8 -topk8`),
/// used only to test key parsing. They protect nothing.
enum KeyFixtures {
    static let rsaPKCS1 = """
    -----BEGIN RSA PRIVATE KEY-----
    MIICXQIBAAKBgQDfvOmC+cXb6+JOsY7AX/aT8oTJNZBffIpf56u6Ed6S3HAYZPsZ
    WhzDQXonofy5jdjVDrbYAjoBpmg3Kf6ewifCxMJF/aM6xtyZFgix0W4pr81DmSHj
    DAWRoemGDxMLCUW0P/J7Rx7jKyLeOtfYgCw36DgzWmnEqk6acPGmdIvYvQIDAQAB
    AoGAb459C/5IJlid9UjY+ZiflLMWfFDF1KZ/RER6PoOMa8OX7MHk5YYl00GBi+Nb
    5JUSaWE1z8yEabA0RFUfDL+YHLlc0dbANNgZiAlNNrZCa/hQb5lamdcG8lDHHbEV
    r/vOXLCZMgp8xPOasGF4tNx4dx1eWA6+ocRxzY47a0ztUqECQQDxb4Hw8IA9ZPCO
    2gsz/3/8XZymtlW/tGmXy0yqlIFUlvOQyH43QXbU0d1Nm1iPvLXh+IxknETs8d1K
    zxgt7/91AkEA7TwZiFSIYTJG7iFXH/kzvBj/g86DQ+2elvN8EX7FvF+HXn7eIIGi
    M906pfOlesUzxvO8Iq4GVubBRP6LnAVTKQJBAKATSpgxHRWXo1dTgjILqRPuDTsL
    kXb1az1VcZN3uvgWzY6lH+oK4XrhWlVmNiW2/PDCFnNumNRw2EQNYv6yLTECQQCI
    9CshQvvAcM/15RVxBeJR8nR/2RZ6fpERP9seFnt8tgFmEgOV7ICBRHP5HeEoMPQl
    JMw1kRVj0a89b12VYpWRAkBRCPpAKlqhKlEQtGTWqIK08d+XudFjZrhYILwrMkrw
    xG99lG8Xvz0WFA5IxylSCc3LY2TdufMAGHIS9w8TETN3
    -----END RSA PRIVATE KEY-----
    """

    static let rsaPKCS8 = """
    -----BEGIN PRIVATE KEY-----
    MIICdwIBADANBgkqhkiG9w0BAQEFAASCAmEwggJdAgEAAoGBAN+86YL5xdvr4k6x
    jsBf9pPyhMk1kF98il/nq7oR3pLccBhk+xlaHMNBeieh/LmN2NUOttgCOgGmaDcp
    /p7CJ8LEwkX9ozrG3JkWCLHRbimvzUOZIeMMBZGh6YYPEwsJRbQ/8ntHHuMrIt46
    19iALDfoODNaacSqTppw8aZ0i9i9AgMBAAECgYBvjn0L/kgmWJ31SNj5mJ+UsxZ8
    UMXUpn9ERHo+g4xrw5fsweTlhiXTQYGL41vklRJpYTXPzIRpsDREVR8Mv5gcuVzR
    1sA02BmICU02tkJr+FBvmVqZ1wbyUMcdsRWv+85csJkyCnzE85qwYXi03Hh3HV5Y
    Dr6hxHHNjjtrTO1SoQJBAPFvgfDwgD1k8I7aCzP/f/xdnKa2Vb+0aZfLTKqUgVSW
    85DIfjdBdtTR3U2bWI+8teH4jGScROzx3UrPGC3v/3UCQQDtPBmIVIhhMkbuIVcf
    +TO8GP+DzoND7Z6W83wRfsW8X4deft4ggaIz3Tql86V6xTPG87wirgZW5sFE/ouc
    BVMpAkEAoBNKmDEdFZejV1OCMgupE+4NOwuRdvVrPVVxk3e6+BbNjqUf6grheuFa
    VWY2Jbb88MIWc26Y1HDYRA1i/rItMQJBAIj0KyFC+8Bwz/XlFXEF4lHydH/ZFnp+
    kRE/2x4We3y2AWYSA5XsgIFEc/kd4Sgw9CUkzDWRFWPRrz1vXZVilZECQFEI+kAq
    WqEqURC0ZNaogrTx35e50WNmuFggvCsySvDEb32Ubxe/PRYUDkjHKVIJzctjZN25
    8wAYchL3DxMRM3c=
    -----END PRIVATE KEY-----
    """

    static let ec256SEC1WithParameters = """
    -----BEGIN EC PARAMETERS-----
    BggqhkjOPQMBBw==
    -----END EC PARAMETERS-----
    -----BEGIN EC PRIVATE KEY-----
    MHcCAQEEINBd/XSJb3stpdvI5jH8hUBc/tfqTt31Ao+ybfF87qDMoAoGCCqGSM49
    AwEHoUQDQgAEi+hJm7OU1ac7EGf2QD3DONdTO738aSzBtX+wETJIf/hbvSAUe9cu
    3sR64lvbuLh0bp7NJMfkStv0acTePvLWrA==
    -----END EC PRIVATE KEY-----
    """

    static let ec256SEC1 = """
    -----BEGIN EC PRIVATE KEY-----
    MHcCAQEEINBd/XSJb3stpdvI5jH8hUBc/tfqTt31Ao+ybfF87qDMoAoGCCqGSM49
    AwEHoUQDQgAEi+hJm7OU1ac7EGf2QD3DONdTO738aSzBtX+wETJIf/hbvSAUe9cu
    3sR64lvbuLh0bp7NJMfkStv0acTePvLWrA==
    -----END EC PRIVATE KEY-----
    """

    static let ec256PKCS8 = """
    -----BEGIN PRIVATE KEY-----
    MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg0F39dIlvey2l28jm
    MfyFQFz+1+pO3fUCj7Jt8XzuoMyhRANCAASL6Embs5TVpzsQZ/ZAPcM411M7vfxp
    LMG1f7ARMkh/+Fu9IBR71y7exHriW9u4uHRuns0kx+RK2/RpxN4+8tas
    -----END PRIVATE KEY-----
    """

    static let ec384SEC1 = """
    -----BEGIN EC PRIVATE KEY-----
    MIGkAgEBBDCCKpTeOKGgTBnGZHHt/etMVqjlX/9fyn7idN2nFmZag62A8KzH2HtB
    3H8X2qBXYqigBwYFK4EEACKhZANiAAQ+/7cji76pkqA2s7dcwOmuNvcdj3cvTqbJ
    mc5z3OZcF3hz0XGPQKR2852P6tN3yBNZYU608FRTP0kB3zENCGYdp6ggjThL7Q74
    xg9c9s183LYn3PPVq8gBzE8tOecN784=
    -----END EC PRIVATE KEY-----
    """

    static let ec384PKCS8 = """
    -----BEGIN PRIVATE KEY-----
    MIG2AgEAMBAGByqGSM49AgEGBSuBBAAiBIGeMIGbAgEBBDCCKpTeOKGgTBnGZHHt
    /etMVqjlX/9fyn7idN2nFmZag62A8KzH2HtB3H8X2qBXYqihZANiAAQ+/7cji76p
    kqA2s7dcwOmuNvcdj3cvTqbJmc5z3OZcF3hz0XGPQKR2852P6tN3yBNZYU608FRT
    P0kB3zENCGYdp6ggjThL7Q74xg9c9s183LYn3PPVq8gBzE8tOecN784=
    -----END PRIVATE KEY-----
    """

    static let rsaEncryptedPKCS8 = """
    -----BEGIN ENCRYPTED PRIVATE KEY-----
    MIIC3TBXBgkqhkiG9w0BBQ0wSjApBgkqhkiG9w0BBQwwHAQI+fl8sGGAOg0CAggA
    MAwGCCqGSIb3DQIJBQAwHQYJYIZIAWUDBAEqBBChc+Cll4whWBCO6hX3dnHcBIIC
    gL0acoRnGNLC/AGVhotC1zpRoOut4EUloKEnIqKWecC/4qcmI7zyUgP2EwF+w2jB
    2faWLDijsfLW+5gYzjS0LP2mxpY8e2d7Lk+rySN1ubn2YdQYNxg4+20BqXqiAYXR
    /vksfe6IRPvBA2Us+paCg3+ErhkxG+Mi3qR+TNWpHysTJYLjFTjZrAielsUUuqtj
    mCtifcrrj+prtsURMWqlxRz/iunezanSWfizqRdVhKsHqJm3Go+9UweAXp9pIUM0
    d6L+0ec68HCr0Ys4aGBrPcmBMoVD2/zcdWl/Q6OuHGiRKa8NZJChItUdqe/2BoPC
    qjCeKCyIcSwX3PwtSXXtaeVl1KVGBB4ZKDZlPoaXbyeiyy5rAY97McX8TpUF3aIO
    s4ob61qQI53X/4DbdzmRwAXNQvU78vFmLgWptR7NEt/oA0KOFSuyOpN78eBbAkDT
    L8jbVJ0B5K8jkWfAuAheMU60gKca5jf6rvCq0HSMyLv72RwI5Hb/R4QNaIxYlO/9
    HAp1hjVIVJ9vi6cXkc7IUiRd6s/ETAihwDigK/QeNRNBR/p9uiPHp/luRyQ9bWMJ
    ajLLmu7xHAglebHY2qL8yq6LozjBlsH579io6Sb+a2abMezB3q3Ja6FPX2ixrnOy
    MtmXEE+ziaBB0lR00qgqfcCMWiNQ7XuvmOfq5qZOMT7VVDU1epnMrzdszNo044RM
    aeI9isqW+vb6Da3M7KKWFgDWffL6CJi9SW+kalRGzJkZuW3LID1X5Cgz+N68vl4K
    TmTd+GEW2YsgChNHGpcCywo7tdfA1CGZ79y97SnhrqQhMqiIVJFNS/1qx02uvSs7
    cL30LZo/b1qjWqSwnGzHz0k=
    -----END ENCRYPTED PRIVATE KEY-----
    """

    static let rsaLegacyEncrypted = """
    -----BEGIN RSA PRIVATE KEY-----
    Proc-Type: 4,ENCRYPTED
    DEK-Info: AES-128-CBC,A3E0C63BE47DCDC4AEE7FA76F683EB5C
    
    +M5HgUJcdZpjHdC0hLqpVtF3f7uavBUXtvcfLvCeBnBa8uaSfQ2ZLwd2YSipWOmt
    ROr/HY6aaUDeTQz8afKSPiJYuHb7LBuX/ryI5NmAjNp3GT2hByaSlQmi+m3oHLwS
    Z4Wp/EO43GUwjlevmw5tSRMo5jn7k7FFzAmogQIGr6Sm/qmZxIqGl9xIbPazh041
    HFsjAd6GR0UfA2DVipQMHS3FGQM5BXRISVRPGFGSZMg1LRER/IlXS2+9azNCQTx8
    9OKC/u5lhZAYws5mMXH0DsaswH5QK8aseSsFefxLqX4SwO/G5IMKI5+rpOPNokB8
    dZWi4cbkeglLNfi17DYUweH6k1s8emksGR6t7j28qo4GmI6P5r2S2IAWpWHaims4
    o1oqftUWrEF1D85xsgXpiBO1D+/zzWu1FI9jn4JwPPU8wZ+QydhIZxpOW4bkFuCb
    Ehj2T41dd2+rmdry6vfOxPdvUyAVfPeeys5NGqbD8zzT/RX+C/0ezBHfVr5Idj4C
    FmSw2RfTi1fFqGv70PHOO/c6Lu0+jSlzs928t7GDpc/8O73PVq7FbTynLgZPzp7a
    cSxmOZ+kwZhRUaGdzRicP7N5v8acLj8wn2YqTilG9k6SyYCrWKCEMseXEopO6Nou
    E1PWrGMWrQTllwXsqB9xgkl5K1OJjhxYMKEdxoMVthZlaP38n6ky3szO69UlhpW2
    AukHDgGK/c+MRsStJGgvZbkvO9OMDdS/chU8dWijHrwJYBqDZ0aSFXtGfNDn5Kvh
    0rug+/HxCt+J3WYNe4AJ2UmJnjY35mgqmSYluHtiyHIKE4iGDRMgKH1MNAoYyuZc
    -----END RSA PRIVATE KEY-----
    """

    /// Public points from `openssl ec -pubout`, hex.
    static let ec256PublicPoint = "048be8499bb394d5a73b1067f6403dc338d7533bbdfc692cc1b57fb01132487ff85bbd20147bd72edec47ae25bdbb8b8746e9ecd24c7e44adbf469c4de3ef2d6ac"
    static let ec384PublicPoint = "043effb7238bbea992a036b3b75cc0e9ae36f71d8f772f4ea6c999ce73dce65c177873d1718f40a476f39d8fead377c81359614eb4f054533f4901df310d08661da7a8208d384bed0ef8c60f5cf6cd7cdcb627dcf3d5abc801cc4f2d39e70defce"
}
