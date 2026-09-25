# playmmix

<a href="https://playmmix.2ad.com"><img src="docs/img/playmmix.gif" alt="playmmix stepping through &quot;Is 13 prime?&quot;: registers change with each step, then Run halts with $0 = 1, prime" width="600"></a>

Write, run and single-step Knuth's MMIX assembly in your browser, on a
desktop or a phone, with nothing to install.

**[Try it: playmmix.2ad.com](https://playmmix.2ad.com)**

## On your phone

playmmix, a full assembler and debugger, works on your phone.

<a href="https://playmmix.2ad.com"><img src="docs/img/playmmix-phone.png" alt="playmmix on an iPhone: the control bar pinned at the top, above the editor" width="300"></a>

## Examples to try

Paste a program into playmmix, or click its link to open it loaded. Each
halts with its answer, printed or left in a register.

### Say hello

Prints a greeting. Change `world` on the `Name` line and run it again.

```asm
% Say hello. Change the name on the next line, then run it.
        LOC     Data_Segment
        GREG    @
Hello   BYTE    "Hello, ",0
Name    BYTE    "world",0
Bang    BYTE    "!",10,0

        LOC     #100
Main    LDA     $255,Hello
        TRAP    0,Fputs,StdOut
        LDA     $255,Name
        TRAP    0,Fputs,StdOut
        LDA     $255,Bang
        TRAP    0,Fputs,StdOut
        SET     $255,0
        TRAP    0,Halt,0
```

**[Open in playmmix](https://playmmix.2ad.com/#p=nY7NDoIwEITvPMWKemtMNeEuf8JBxQAXT6YJDZCUYrBEfXvZakCiB2Mvu51vJzNzSNgdCi5EvQC3YDLnoAoOklUcavnc-U2BKCUn-JXQtBJKtTDg9baRq6fHFDslPK-4VD0MYj_AuTZCDOk255j6qJhaIGASauwx7p1d60ZkSJyu0ohMTLKkHfiIny4pNXaslFr0bC3OVpZFdFB_n8b2ASclm3OrLiRRWdQOhUdO7PWXEWv_akz8dDDSL66QCdWBBw)**

```
Hello, world!
```

### Is 13 prime?

Trial division: divide `N` by 2, 3, 4 and so on until a divisor turns up or
its square passes `N`. The answer lands in `$0`: 1 for prime, 0 for
composite. Change `N` and run again.

```asm
% Is N prime? Trial division: try D=2,3,4,... against N; D dividing
% N evenly makes it composite, D*D passing N first makes it prime.
N       IS      13             % 0-65535; SET rejects anything wider

        LOC     #100
Main    SET     $1,N
        PUSHJ   $0,IsPrime      % $0 = 1 if prime, 0 if composite
        SET     $255,0
        TRAP    0,Halt,0

IsPrime CMPU    $1,$0,2
        BN      $1,Composite    % 0 and 1 are not prime
        SET     $1,2            % D, the trial divisor
Loop    MULU    $2,$1,$1
        CMPU    $2,$2,$0
        BP      $2,Prime        % D*D > N: no divisor found
        DIVU    $2,$0,$1
        GET     $2,rR           % N mod D
        BZ      $2,Composite
        ADDU    $1,$1,1
        JMP     Loop
Prime   SET     $0,1
        POP     1,0
Composite SET   $0,0
        POP     1,0
```

**[Open in playmmix](https://playmmix.2ad.com/#p=bVHLbsIwELznK1ZquVQuckLTA4hWgKsCghDx6KG3iBhwC3Fkp1T8fdd5OEHUiuTYnp2ZnW3BREMAqRIn_gprJaIjxOIstJBJFzJ1Adb3SIc8kXa7DdE-EonOIOgBy2GxSPZOCwn4mSfHC5yib65BZLCVp1RqkXEC7IFBGmmNUATuhEICi8uF204AxZqsit3tQHO1gD4--37H78HqbQ2Kf_FtpiFKLtnB0P6KmCvHqeCzxSjf71xKnTlaNgdTaNa9SwKLDDer8dRcUjLRofFSCd5T6IMLYldYJEDNv23LMlhaz_cJtdfr5SA0OyXj6Jjhg1Pxj-bhpvSBqp6tGJYZ4P2oUil7x0Zj9BIpDoksM7s14BLvOjNGIDtwHKIdqlTOTMrUPM83s8KGR4wT1_JZf_iAX93SMCwNeqSRVC6EE36BoIvuKh3YyZ8ktrVs8mFJaVPt3cZH1PLKfQAnGQOr5T-t_OhmCgPGbKguqemn88K06dqpXNvEaAMZLgqki7Oq8y-gCKT_Av8A)**

Run it and the registers pane shows `$0` = 1. Click **Step** instead to
watch `IsPrime` try each divisor.

### On what day will my birthday fall?

Prints the weekday of `MONTH`/`DAY` for `YEARS` years starting at `FROM`,
skipping any year without that date. The default, 29 February from 2000,
prints only the leap years. Put in your own birthday.

```asm
% On what day will my birthday fall? Change MONTH, DAY, FROM and YEARS.
MONTH   IS      2               % 1–12
DAY     IS      29              % 1–31
FROM    IS      2000            % 1583–9999, the first year
YEARS   IS      20              % 1–100, how many years to print

        LOC     Data_Segment
        GREG    @
MonthDays       BYTE 31,28,31,30,31,30,31,31,30,31,30,31
SakamotoT       BYTE 0,3,2,5,0,3,5,1,4,6,2,4
Sep     BYTE    " ",0
NL      BYTE    10,0
YearDigits      BYTE 0,0,0,0,0,0
Sunday  BYTE    "Sunday",0
Monday  BYTE    "Monday",0
Tuesday BYTE    "Tuesday",0
Wednesday       BYTE "Wednesday",0
Thursday        BYTE "Thursday",0
Friday  BYTE    "Friday",0
Saturday        BYTE "Saturday",0

        LOC     #100
% Load the plain (non-leap) day count for MONTH from a 12-entry table.
Main    LDA     $20,MonthDays
        LDBU    $1,$20,(MONTH-1)

% IsFeb ($2) is 1 when MONTH is February, else 0.
        SET     $7,MONTH
        CMP     $8,$7,2
        SET     $2,0
        BNZ     $8,1F
        SET     $2,1
1H

% YearAdjust ($3) is 1 when MONTH is January or February: Sakamoto's
% method treats those two months as the tail of the PREVIOUS year.
        CMP     $8,$7,3
        SET     $3,0
        BNN     $8,2F
        SET     $3,1
2H

% Sakamoto's per-month offset for MONTH, from a second 12-entry table.
        LDA     $21,SakamotoT
        LDBU    $4,$21,(MONTH-1)

        SET     $5,FROM
        SET     $6,FROM+YEARS
        SET     $23,400         % an immediate operand holds only 0-255

YearLoop
        CMP     $8,$5,$6
        BNN     $8,LoopDone

% Is $5 (the current year) a leap year? Gregorian rule: divisible by
% 4, except a century year (divisible by 100) unless also divisible
% by 400.
        DIVU    $9,$5,4
        GET     $10,rR
        BNZ     $10,NotLeap
        DIVU    $9,$5,100
        GET     $10,rR
        BNZ     $10,IsLeap
        DIVU    $9,$5,$23
        GET     $10,rR
        BNZ     $10,NotLeap
IsLeap  SET     $11,1
        JMP     LeapDone
NotLeap SET     $11,0
LeapDone

% Does DAY exist in MONTH this year? Only February's count moves,
% and only on a leap year.
        SET     $12,$1
        BZ      $2,SkipLeapAdjust
        BZ      $11,SkipLeapAdjust
        ADDU    $12,$12,1
SkipLeapAdjust
        SET     $13,DAY
        CMP     $14,$13,$12
        BP      $14,SkipYear

% Sakamoto's algorithm: weekday = (y + y/4 - y/100 + y/400 + t + day)
% mod 7, y adjusted for Jan/Feb, weekday 0 = Sunday.
        SET     $15,$5
        BZ      $3,NoYearAdjust
        SUBU    $15,$15,1
NoYearAdjust
        DIVU    $16,$15,4
        DIVU    $17,$15,100
        DIVU    $18,$15,$23
        ADDU    $19,$15,$16
        SUBU    $19,$19,$17
        ADDU    $19,$19,$18
        ADDU    $19,$19,$4
        SET     $13,DAY
        ADDU    $19,$19,$13
        DIVU    $9,$19,7
        GET     $22,rR

% Print the year as decimal text: divide by 10 repeatedly, filling
% the buffer from its end backward.
        SET     $30,$5
        LDA     $31,YearDigits
        ADDU    $31,$31,4
3H      DIVU    $32,$30,10
        GET     $33,rR
        ADDU    $33,$33,'0'
        STBU    $33,$31,0
        SUBU    $31,$31,1
        SET     $30,$32
        BNZ     $30,3B
        ADDU    $31,$31,1
        SET     $255,$31
        TRAP    0,Fputs,StdOut
        LDA     $255,Sep
        TRAP    0,Fputs,StdOut

% $22 is the weekday index, 0 for Sunday up to 6 for Saturday. Walk
% it down by one each time it misses, until it names its weekday.
        BZ      $22,PrintSunday
        SUBU    $22,$22,1
        BZ      $22,PrintMonday
        SUBU    $22,$22,1
        BZ      $22,PrintTuesday
        SUBU    $22,$22,1
        BZ      $22,PrintWednesday
        SUBU    $22,$22,1
        BZ      $22,PrintThursday
        SUBU    $22,$22,1
        BZ      $22,PrintFriday
        JMP     PrintSaturday
PrintSunday
        LDA     $255,Sunday
        JMP     PrintWeekdayDone
PrintMonday
        LDA     $255,Monday
        JMP     PrintWeekdayDone
PrintTuesday
        LDA     $255,Tuesday
        JMP     PrintWeekdayDone
PrintWednesday
        LDA     $255,Wednesday
        JMP     PrintWeekdayDone
PrintThursday
        LDA     $255,Thursday
        JMP     PrintWeekdayDone
PrintFriday
        LDA     $255,Friday
        JMP     PrintWeekdayDone
PrintSaturday
        LDA     $255,Saturday
PrintWeekdayDone
        TRAP    0,Fputs,StdOut
        LDA     $255,NL
        TRAP    0,Fputs,StdOut

SkipYear
        ADDU    $5,$5,1
        JMP     YearLoop

LoopDone
        SET     $255,0          % a clean exit code, not a stray one
        TRAP    0,Halt,0
```

**[Open in playmmix](https://playmmix.2ad.com/#p=nVjbUttIEH3XV3RloWIqA9HF5pKqrSzE3FKAKQyh2JetwRpjLZLGJY1i_JZ_2D_cL9nuGV0tGWpxSjaa093q6cuZVjZhFMNixhX4fAmLIAwhWsJjkKgZLUx5GH6FbzMePwm4HF3dnjEYHj4wOLkZXQKPfXg4PrwZ71gaA4DzMeiPC83PJjj__vrHcS3U1gul4EGXoOdY-gl1Qdu2VwQH-x7KHuCHgZoJmAZJqmApeGJptxranf7YNoOZXEDE46VWTEFJmCdBrCyrkL0YfdO_Q674X2PxFAlEC_D05viUfv-wLmWsZkO-THPk6OH2GDyHufsMvz279t24tcb8mUdSydu6IiLMZQNGvwPmsD7bxfu-NRbzSgg_H-ADs62rC2isOjYuPuB-hsFToNKG3fKfNc5iSnJlyyyQQdxMEzILBN1mIiWshPIFwu6FHxu09sgP5apWn2VJTSIXKVZJ4iQJms82CwSNucqSlnKxShKtrP2GSbY24UJyXxfJPORBDL1Yxtuh4PMtXfgTmcUKpjIxRQ7TREbAwXG3MdfJEhR_DAVWOamS7eGhtr3h2qxMe_Xk4dGdRh1GAj1tctvZstCN8_REPEJvw92CIAUHW0_E-TPxHrEk48mSgQhTAfZOaXN8bKpjY49p6RL4dnltgH2GmNtWcDEoZbiu_iyEnZMuUcdyzshNKp1D_-8M26m34XX6-p3H5CpgzAq3v0BRyh9TNBIJNZMY9ERwLEH8G7ekFhIiilgKPNX5UDwIQU7139c3xz_OR3dj3Yo7a_botR33Gnu8KoTdky5Rx3L1HitfYS6Sbe0VOjJNRa0SWFEKqZhgB7Qqosp5UREOK_u5XRF9RgK1imj5N2BEfO31Xb3-SfNaR-Y81q_R4yZSMwRRJPyAKwES90dcPZOhn4KMwyXY2-5gYGmKuJBy3hnqAdvY7QorKQxlLEw9o8vQo-RNsiTB2OjcbWHEqLv0zVc4TcSTTAJ0KslC8QX84GeQBhhAeFyikT7W-8tEzBVqTdBElhgyhl5dEEnN3oIsDkWKxROmsjKDNhDHCFQJGZ7_MCE_oI30K74uQoYUmdy0OwOXr6S6QN_XmCI6-R_GztNXbGHe3uOYsVlLv-NgVRca3_MUkoxOU67WELetEsbgDaVI6VzHNATY8kHR5mqGjW5SOKKqKRodW8YwZiR_ipRZm3oU0IUl43rqOwjMcdlG5eyR2R1xz_g5mJNThnfaEuj1GpHD4fCusk0stkawcsJjuN121TvYoIihker511BiZJZaZoU-eEjVrWbRF1gI8UwHyu_QW8InWH7uwzZ-Y9GYO_2r8EKhLeJIJMg9Bkvg2lPha-pBbv2MsWalORsNmsO5K6JYSIN2vDysl4rHK7W74nRCNbwcq1OsLFRnV4v1O5A9Y6DWEBW2r7F6gVdZOjCYs9vhFGF07a3Ro2t_PdZ_M9dtc15ndyK0125O16XmxMRd04yoDy3NVHiY-WISRDwEJV6UoTg_py1IxBzPQOGHeLJPccYO4ic0QcqP2XQqEnPI0KAmsI8e-eR5wRO_I9U4MNZSXR46OEpWw157pwjT1be8s5V9etgwaNPpoDTPq9NQZQu7A6-P9sfKu9ujGubUzuIyrbkHTveOPLdNdzQaH63dSochPM8IKoHbm0PduTY7mWcqZWPljzLVcWCjHg7Vb-lhvjD5NPhQ2oq-DGJfvDBsT-rafJzO5vQOsWuW8sl0B-55-Iw2AnzRkouY6gK5FwSfzEAFkSAgCtIU2RSPOIUjES7EPEJeprLIn7fTwZsu06VoHt6OPOJ0Oes1zWT_Hs188H-PavlK8K7n5i8L79E17xKtw9LEMM-W1RXRZsE0sYaVe5MrfbZ2BbhhaAV73dBqvBuWVsHXTbXj3zDWht_wbDUjTddW0deNraSoYeq19LUMlfnszmEj23Xl91DI1cWbDFJODy1WG-jBsrWrcjq3ypG7k_bs-v9r4AiN41dMo5zCKc0XDGJJk3WqEq5Zp8PRMx4q5O3_AA)**

```
2000 Tuesday
2004 Sunday
2008 Friday
2012 Wednesday
2016 Monday
```

### Digits of pi

A showpiece: an MMIX program that computes `DIGITS` decimal places of pi
with Machin's formula, doing arbitrary-precision arithmetic on arrays held
in memory, each array standing in for one number too wide for any
register. `DIGITS` runs 1–1000. At 334 lines the source lives behind the
link.

**[Open in playmmix](https://playmmix.2ad.com/#p=7VpZc-JIEn7Xr8iYxeFLpiUM-JjwxmDT3eMJHx0Gr2P6oTdkVIDCoGJ1-Nhfv19WSULowHasd2cehqBboo6sysyv8ipvUN-beFFIckwLr0lnU8efCOqffz0fDuhezOSTSTZFkmzLsprGhrFBl85o6vmbIY1lMI9njomZdEJ2d8cJRpHjb9mfOtu0R-3l79b-0bZJURz4IQaDhueDZPQkKRSBJ0JyaCTnizgSAY0cn8J4TuLZGUWzlyZ9xnrkx_N7dKodkReCRCQlPXmu4G2Q9AUFYuKFoGBSKMmLaOY9grLn01zMZfCCN_Q67jFhAScInBcQAdt39CQDNzTp3gmFYlN_aMv3QNUVI2_uzMjVclpgEzwe7MxlGIFE6E18b-xh25HqobEXhFETgn0cYOLMpMt4lrz1XPecOXd8lwbxPb-DgmbKCQTNpD_BSo9e6EkfK8SzyFvMQDtSvx3X9fhNzQ_j-yiAiPCbGfEpmjqRZsxU8lCb4SaKvLmgvT2MEBQ6eHcCL5rOReSNKHLiyTQiRWThLFh8IxDxhEsyTji6f9HPRIIsNSUO7lAvTSNBDJ3jH38gR-Prbe-mT7m2jjG8HvYu-ssmPW3X3lVjjTvdmnVv6fG7h9ufjoyL3mB4_eVL1nm4s3W3Z28bRjKJLq7PNFEncv45EJO58KOs8-vN56_8_MU4hZ7PJFhJeq7Phr2c3o1v8sm20nbNiv6YduEleepHtlJ-Hnfhn2kzVRH0gqC02192D3fujKEI5vW9g3j-TYZr-6_EpL7_RoSAUqd-QF-MTuNxRWeioJZxwYrH5_T34Wd-_rTf_Mm0ysL_G2v-0sGxw-e3y2-qkX8PIieIDKBsDx_6LgLZY6xuAdTBScPaPoYtiNLTyLhtWGx3_o2BTSMbnq03-DxUz0bLtMqN--ad8f1CN55-p7SRyfRxNsoT2nkqw-u0sWGZjVbW0ev3b5M18T1cTrg9vU1WwNfO2lP2v18YpZW_Xes-i4WYCOVMLl60UFwRRhAKTFkwOmnYEA630PEJN5iplJpGNqNKBFY1m2eJXM6S3TW6ppJXOvj06iptZ_Iru77oJ6LpmA3bbLTLMusombXLMoMw2zmZZe01Mju7MEqrV8nsPGTR5lAUCO1nbPLGBKu7hBQMLKPJJDGDpYc303Nfl9mdcZ6AKcNS29STfxdhpXAgg_2cRL-nHXrWlayRQwWmlNzK8sGOjPIWsk1bVVPU8BsRGaVd5OZZxnJcUfB2TvCpf0tFb2rHJQONV26lTydpY4ZZE_PZc-5V-00T-pvDWrgqEtB-SAcL8F2-eNZj4W-S1XPC72kODszMwJc1c2Dia73ZiFQ0doGG_qCAhq6J_dQelBUsXN5e0NKGNA5qjEujk3X0z_9xm5JCx1KvX5e7D27qTuL-25EGO9CtgA24NRL2Kk9gGt4sgZDGLSKPhZ2TXPsKHGbCWYMHRsELLQK5cCZOBDhE8slBNwNiHEgfwUe6g_8tGO6qTEMSlBiXRUjsm9jVOki0y5CAAjp5BWcaU5rJOaIMEi3VcVCGRGcNJNo1RubtDg3cGgl7lZBI4tw6N7a76sbMBAH1wbSRBs6ZYo_WKfbIxPeDFNs7LykWm3mbYrOObsFbZoo9UBrvVnYc5DWeOetD1XG0nrnTK0pHY7NX8ozPUFm7B_W0bKNq4nB5cj4ERxCukUizEkdJjlSHoz2FI1Ld9Hf9wwmRPArkZ6BRwFcduJJV_gBwDcrgwmY-ClxFq5HH0ArqajB0lY7Gnq7kqQwC-VRepaOI1YCoamYGik5xGx9tpiBeI5FnDbzmlNUonrdpBznbj62jHU4rTXpGiSP8V8xhI7IZyFgHIstcDIGM58chZdkXMvQZChA-0nXk8Qw3IDaSiwX_lP5I6Ax8Gnj-AzctOCcEmZjDnTGKAOEy41mmTCWndmSW88Fvt4Nff9PaKudKqzMLmeJbZ5YS2HdMtK1Xz1E2AdlyaakMGZhkW1WzKvaXAREzOlW7zmLIyvCZcp8NTkZPqPWwCxSwDqHlOeAhXReWyJfAgRPJqqVNq0DHBh2LIyLKsMQVtmUD1GMMVFWscyHlAvBQnNHUCRk6sf_AA30ZoQg3OSbxKBAezRwunfGuYNnimavoMZCQSHOtbOqhdBWglKULS6GOp5PiG_pcHJHme7Wu84RylgODofevDNkGDXl1tTc8NTOcFbhCVZa2WKrbP-smPhVcC1y80IMQi2Q4FwyjUMzGfAJR8lIlvywh8EBaFcgqGSgVVtbhLMdcObNeQzQPtYa1Fmu5-mla-0xsBl7ZZBxrLGQyg7Kfpt5oyoomqB9VyYkqf0aoSrLqYi2HZlkRdqoIxtSbzEheOkUmc9wk4VjZ4OrlemPsHGOMV5cvVq3et3xxuQ0aKNuagAzg6vxoqWLpXD4KymeRhZOrK6pssEEkQKnrZRVjrInmu61Oa73ZKblSBo_ZqvakJZ9nF7LBgg4S26FVvOKoGDS5Sv3P6qRxwdwJuZwOX5crF3J1GpLg6noc8umESMKsiozjEWaCUmX-FNSqpt008obg3QBc66vS8nkF1apq5yrZeue5PPZ_-d0P8LtAxHrPu0HFS6L0jiK1jvje8-l9wpUFPZ9g1DHhvx-tk86B3bKb62tfeU-coBGT1eH4YG8HsnUw_7P5oFpHAR7-v64CCxacxdotfIS7yC_57jCSEfeH2fQUuK_i7QPMKt8GvuVWF7c2I2cmSPAlre5RZ5ivE3Gle89XqCpgxl2vSp-Wxrn5RtudV4Ddrdp0ff2vShZ5eu33kvtvfEtOtir9_PysLnDT2_bCTbMzViE9ZLeQkBzXOuernnmLO1HaUFHz5v4mO3bVpCLimfbB2yq0WQRMAp3z5iqNqZy5fCePcDRZV9168x21qpGoXEFf9iJ1CXkAAgRVPWnYn450erGQob6XxpC5dOlIGWuMwyzwqS9UF97oIdSX1JyPa5q4Ym5W2Wq76sBoOZXlvm8m15fqbxlWbHtWWMFsxd7KkcnKqG0uoRxRLk1rA_vJnbcrnnErcYOGjE_F2mtF14ybrnlIK0mguiHG5bR2cAiuDhGrcvzvLPIJf74uzDvsVtW3KwF5nbomrtR1c2wdggulAuZtM1G2Vnx5WVsXaKrWrY0u2PlwsNDJlrQ5c-a6yiGsRyrB7bIObBi_Qw4zKiQIY2lVzEDZh2fZrbIq7DZ0kZe5UqAKLnBaZin-nlALFG7Zatss77a5aW3mYqHTZV_-aqjuJnNJzFxFdGrt1UVPxX1LhuIKyA5yDOZFVdxcWWWtTsfkS_ysZ3jTUwta5pdFHIXmIHKv46h6YuHvA16ZujyzmGpVzPoVOS86_gM)**

```
3.1415926535897932384626433832795028841971693993751058209749445923078164062862089986280348253421170679
```

## A short MMIX primer

MMIX is a register machine: 256 registers (`$0`–`$255`) and a small set
of instructions, one per line. A line has an optional label, an operation
and its operands: `Main LDA $255,Hello` labels the line `Main`, its
operation is `LDA` (load address), and its operands are `$255` and `Hello`.
Comments start with `%` and run to the end of the line.

The hello example, line by line:

- `LOC Data_Segment` and `LOC #100` place what follows: fixed data in the
  data segment, code at `#100` (hexadecimal), where every playmmix program
  starts.
- `GREG @` gives `LDA` a base register for reaching data-segment labels.
- `BYTE "Hello, ",0` lays down a string's bytes plus a trailing zero, which
  marks the string's end.
- `LDA $255,Hello` loads the address of `Hello`'s bytes into `$255`. `TRAP
  0,Fputs,StdOut` prints the string at that address to standard output.
- `SET $255,0` then `TRAP 0,Halt,0` sets `$255` to zero and stops the
  machine. `$255` doubles as the TRAP argument register and, once halted,
  the exit code the machine pane shows.

`IS` names a constant without storing it. The prime and birthday examples
set their inputs this way: `N IS 13`.

For the rest of MMIX, the [instruction
reference](https://mmix.cs.hm.edu/doc/instructions-en.html) is the full
list.

## A tour of the screen

| region            | shows                                                | updates                                         |
|-------------------|-------------------------------------------------------|------------------------------------------------|
| header            | the title, New, the run controls, the run-state label, the status message and Share | the run-state label and status message, after every action |
| editor            | the MMIX source with a line-number gutter; click a line number to set a breakpoint | live as you type (debounced), and its current-line marker while paused |
| output            | the program's stdout, stderr and diagnostics, in the order they arrived; `exit N` once halted | at each pause, and at every chunk boundary during a run |
| machine status    | the program counter, the call depth and `exit N` once halted | after every step or run segment |
| registers         | the general-purpose registers in play: the current locals, the current globals and any register that has ever gone nonzero this load | after every step or run segment, changes highlighted |
| special registers | the CPU state registers: `rA`, `rG`, `rL`, `rJ` and the rest | after every step or run segment, changes highlighted |
| memory            | the loaded text and data segments in hex and ASCII, aligned to 16-byte rows | after every step or run segment, the current row and instruction highlighted |

## The controls

Run, Continue, Step, Next, Interrupt and Reset sit top left on desktop; each
button's bold amber first letter is its keyboard shortcut. On a phone they
move to a bar pinned to the top of the screen, and on a full-screen iPad to
the bottom.

- **New**: start over from the minimal skeleton, asking first when there is
  work to lose.
- **Run** (`r`): restart from the start state and run to a breakpoint or
  halt.
- **Continue** (`c`): resume a paused run in place, executing the
  instruction at the PC, then running to a breakpoint or halt.
- **Step** (`s`, `F11`): execute one source-level step, following into
  calls.
- **Next** (`n`, `F10`): execute one source line, running any call along the
  way to completion rather than stepping into it.
- **Interrupt** (`i`): pause a Run, Continue or Next in progress.
- **Reset**: reload the current source from the top, clearing output and
  highlights and keeping breakpoints.
- **Share**: put the program into a link and share or copy it.

Run always restarts from the top, even mid-program. Continue instead picks
up exactly where a paused run left off. Reset also returns to the top, but
waits there instead of running.

Press **Ctrl-S** (**Cmd-S** on macOS) anywhere on the page to reassemble
immediately instead of waiting for the debounce.

## Good to know

- The program is kept in this browser as you type. iOS Safari can clear a
  site's storage after days unused, so treat it as a convenience.
- **New** and opening a shared link both ask first when there is different
  work to lose.
- A shared link carries the whole program; no server holds it.
- A program can print, but playmmix has no keyboard input.

## Learn MMIX

- [Knuth's MMIX page](https://www-cs-faculty.stanford.edu/~knuth/mmix.html):
  the introduction, the Fascicle 1 tutorial and the instruction set.
- [mmix.cs.hm.edu](https://mmix.cs.hm.edu/): full documentation, a visual
  debugger and example programs.

## Build and run locally

### Run it

Install [Trunk](https://trunkrs.dev/) and the wasm target:

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
```

Then, from this directory:

```sh
trunk serve
```

Open `http://127.0.0.1:8080`.

### Debug it

`cargo test` runs the Rust-side unit tests (editor, control and machine-pane
logic) on the host target, with no browser. `docs/layout-spec.md` is the
normative spec for machine-pane layout decisions.

### Deploy it

playmmix deploys to [playmmix.2ad.com](https://playmmix.2ad.com) via CDK;
see [`cdk/README.md`](cdk/README.md).

### Contribute

See [`AGENTS.md`](AGENTS.md) for this project's coding and review
conventions.

## Relationship to checksmix

playmmix uses [checksmix](https://github.com/jac18281828/checksmix) as its
MMIX assembler and interpreter: instruction decoding, the register file and
TRAP handling all live there. playmmix calls only its public API
(`MMixAssembler`, `MMix`) and builds the editor, the run/step controls and
the machine-pane rendering around it.

playmmix requires checksmix 0.3.13 or later and reads MMIXAL exactly as that
release does; see checksmix's own `CHANGELOG.md` for the language rules.

For a command-line debugger and `.mmo` object files, use
[checksmix](https://github.com/jac18281828/checksmix).
